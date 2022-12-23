// Copyright (C) 2022 Daniel Mueller <deso@posteo.net>
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

#![allow(clippy::let_unit_value)]

use std::borrow::Cow;
use std::ffi::OsStr;
use std::io::Write;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;
use std::process::Command;
use std::process::Stdio;

use anyhow::bail;
use anyhow::Context;
use anyhow::Result;


// Redefine standard print macros to cause a compilation error on usage,
// in order to force output to a provided writer.
#[allow(unused)]
macro_rules! println {
  ($($arg:tt)*) => {
    compile_error!("attempt to use `println` macro; please use `writeln` instead")
  };
}

#[allow(unused)]
macro_rules! print {
  ($($arg:tt)*) => {
    compile_error!("attempt to use `print` macro; please use `write` instead")
  };
}


const GIT: &str = "git";


/// Format a git command with the given list of arguments as a string.
fn git_command(args: &[&str]) -> String {
  args.iter().fold(GIT.to_string(), |mut cmd, arg| {
    cmd += " ";
    cmd += arg;
    cmd
  })
}


/// Run git with the provided arguments and read the output it emits.
fn git_raw_output(directory: &Path, args: &[&str]) -> Result<Vec<u8>> {
  let git = Command::new(GIT)
    .current_dir(directory)
    .stdin(Stdio::null())
    .args(args)
    .output()
    .with_context(|| format!("failed to run `{}`", git_command(args)))?;

  if !git.status.success() {
    let code = if let Some(code) = git.status.code() {
      format!(" ({})", code)
    } else {
      String::new()
    };

    bail!(
      "`{}` reported non-zero exit-status{}",
      git_command(args),
      code
    );
  }

  Ok(git.stdout)
}


/// Run git with the provided arguments and read the output it emits, as
/// a `String`.
fn git_output(directory: &Path, args: &[&str]) -> Result<String> {
  let output = git_raw_output(directory, args)?;
  let output = String::from_utf8(output).with_context(|| {
    format!(
      "failed to read `{}` output as UTF-8 string",
      git_command(args)
    )
  })?;
  Ok(output)
}


/// Run git with the provided arguments and report the status of the
/// command.
fn git_run(directory: &Path, args: &[&str]) -> Result<bool> {
  Command::new(GIT)
    .current_dir(directory)
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .args(args)
    .status()
    .with_context(|| format!("failed to run `{}`", git_command(args)))
    .map(|status| status.success())
}


/// Convert a byte slice into a [`Path`].
#[cfg(unix)]
fn bytes_to_path(bytes: &[u8]) -> Result<Cow<'_, Path>> {
  Ok(AsRef::<Path>::as_ref(OsStr::from_bytes(bytes)).into())
}

/// Convert a byte slice into a [`PathBuf`].
#[cfg(not(unix))]
fn bytes_to_path(bytes: &[u8]) -> Result<Cow<'_, Path>> {
  PathBuf::from(bytes.to_str()?).into()
}

/// Print rerun-if-changed directives as necessary for reliable workings
/// in Cargo.
fn print_rerun_if_changed<W>(directory: &Path, writer: &mut W) -> Result<()>
where
  W: Write,
{
  let git_dir = git_raw_output(directory, &["rev-parse", "--git-dir"])?;
  // Make sure to exclude the trailing newline that git unconditionally
  // emits for the above sub-command.
  let git_dir = bytes_to_path(&git_dir[..git_dir.len() - 1])?;

  // Make sure to run this script again if any of our sources files or
  // any relevant version control files changes (e.g., when creating a
  // commit or a tag).
  static PATHS: [&str; 3] = ["HEAD", "index", "refs/"];

  let () = PATHS.iter().try_for_each(|path| {
    writeln!(
      writer,
      "cargo:rerun-if-changed={}",
      git_dir.join(path).display()
    )
  })?;

  Ok(())
}


/// Retrieve a git revision identifier that either includes the tag we
/// are on or the shortened SHA-1. It also contains an indication (`+`)
/// whether local changes were present.
///
/// This function is meant to be run from a Cargo build script. It takes
/// care of printing necessary `rerun-if-changed` directives to the
/// provided writer. As a result, callers are advised to invoke it only
/// once and cache the result.
///
/// The function works on a best-effort basis: if git is not available
/// or no git repository is present, it will fail gracefully by
/// returning `Ok(None)`.
// TODO: Support reading information from .cargo_vcs_info.json.
pub fn get_revision<P, W>(directory: P, writer: W) -> Result<Option<String>>
where
  P: AsRef<Path>,
  W: Write,
{
  let dir = directory.as_ref();
  let mut w = writer;

  // As a first step we check whether we are in a git repository and
  // whether git is working to begin with. If not, we can't do much; yet
  // we still want to allow the build to continue, so we merely print a
  // warning and continue without a git revision. But once these checks
  // are through, we treat subsequent failures as unexpected and fatal.
  match git_run(dir, &["rev-parse", "--git-dir"]) {
    Ok(true) => (),
    Ok(false) => {
      writeln!(
        w,
        "cargo:warning=Not in a git repository; unable to embed git revision"
      )?;
      return Ok(None)
    },
    Err(err) => {
      writeln!(
        w,
        "cargo:warning=Failed to invoke `git`; unable to embed git revision: {}",
        err
      )?;
      return Ok(None)
    },
  }

  // Note that yes, it is conceivable that we bailed out above because
  // no git repository was found, later the user created one, and we
  // would not run re-run properly in that case. But we'd be random
  // guessing where the directory structure could manifest and we are
  // just not going down that road.
  let () = print_rerun_if_changed(dir, &mut w)?;

  let local_changes = git_raw_output(dir, &["status", "--porcelain", "--untracked-files=no"])?;
  let modified = !local_changes.is_empty();

  // If we are on a tag then just include the tag name. Otherwise use
  // the shortened SHA-1.
  let revision = if let Ok(tag) = git_output(dir, &["describe", "--exact-match", "--tags", "HEAD"])
  {
    tag
  } else {
    git_output(dir, &["rev-parse", "--short", "HEAD"])?
  };
  let revision = format!("{}{}", revision.trim(), if modified { "+" } else { "" });
  Ok(Some(revision))
}
