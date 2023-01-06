// Copyright (C) 2022-2023 Daniel Mueller <deso@posteo.net>
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

#![allow(clippy::let_unit_value)]
#![warn(clippy::print_stderr, clippy::print_stdout)]

use std::borrow::Cow;
use std::ffi::OsStr;
use std::io::stdout;
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::process::Stdio;

use anyhow::bail;
use anyhow::Context;
use anyhow::Result;


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
  use std::os::unix::ffi::OsStrExt as _;

  Ok(AsRef::<Path>::as_ref(OsStr::from_bytes(bytes)).into())
}

/// Convert a byte slice into a [`PathBuf`].
#[cfg(not(unix))]
fn bytes_to_path(bytes: &[u8]) -> Result<Cow<'_, Path>> {
  use std::path::PathBuf;
  use std::str::from_utf8;

  Ok(PathBuf::from(from_utf8(bytes)?).into())
}

/// Print rerun-if-changed directives as necessary for reliable workings
/// in Cargo.
fn print_rerun_if_changed<S, I, W>(directory: &Path, sources: S, writer: &mut W) -> Result<()>
where
  S: IntoIterator<Item = I>,
  I: AsRef<Path>,
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
  let () = sources.into_iter().try_for_each(|path| {
    writeln!(
      writer,
      "cargo:rerun-if-changed={}",
      git_dir.join(path.as_ref()).display()
    )
  })?;

  Ok(())
}


// TODO: Support reading information from .cargo_vcs_info.json.
fn revision_bare_impl<S, I, W>(dir: &Path, sources: S, writer: W) -> Result<Option<String>>
where
  S: IntoIterator<Item = I>,
  I: AsRef<Path>,
  W: Write,
{
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
  let () = print_rerun_if_changed(dir, sources, &mut w)?;

  // If we are on a tag then just include the tag name. Otherwise use
  // the shortened SHA-1.
  let revision = if let Ok(tag) = git_output(dir, &["describe", "--exact-match", "--tags", "HEAD"])
  {
    tag
  } else {
    git_output(dir, &["rev-parse", "--short", "HEAD"])?
  };
  Ok(Some(revision.trim().to_string()))
}


fn revision_impl<S, I, W>(dir: &Path, sources: S, writer: W) -> Result<Option<String>>
where
  S: IntoIterator<Item = I>,
  I: AsRef<Path>,
  W: Write,
{
  if let Some(revision) = revision_bare_impl(dir, sources, writer)? {
    let local_changes = git_raw_output(dir, &["status", "--porcelain", "--untracked-files=no"])?;
    let modified = !local_changes.is_empty();
    let revision = format!("{}{}", revision, if modified { "+" } else { "" });
    Ok(Some(revision))
  } else {
    Ok(None)
  }
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
#[deprecated(note = "use git_revision() function instead")]
pub fn get_revision<P, W>(directory: P, writer: W) -> Result<Option<String>>
where
  P: AsRef<Path>,
  W: Write,
{
  let sources = [OsStr::new(""); 0];

  revision_impl(directory.as_ref(), sources.iter(), writer)
}

/// Retrieve a git revision identifier that either includes the tag we
/// are on or the shortened SHA-1. It also contains an indication (`+`)
/// whether local changes were present.
///
/// This function is meant to be run from a Cargo build script. It takes
/// care of printing necessary `rerun-if-changed` directives to stdout
/// as expected by `cargo`. As a result, callers are advised to invoke
/// it only once and cache the result.
///
/// The provided `sources` should be a list of source files or
/// directories (excluding any `git` data) that influence the components
/// embedding the git revision produced in one way or another. Typically
/// including `src/` in there is sufficient, but more advanced
/// applications may depend on additional data.
///
/// The function works on a best-effort basis: if git is not available
/// or no git repository is present, it will fail gracefully by
/// returning `Ok(None)`.
pub fn git_revision<D, S, I>(directory: D, sources: S) -> Result<Option<String>>
where
  D: AsRef<Path>,
  S: IntoIterator<Item = I>,
  I: AsRef<Path>,
{
  revision_impl(directory.as_ref(), sources, stdout().lock())
}
