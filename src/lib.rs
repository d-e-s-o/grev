// Copyright (C) 2022-2023 Daniel Mueller <deso@posteo.net>
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

#![allow(clippy::let_unit_value)]
#![warn(clippy::print_stderr, clippy::print_stdout)]

use std::borrow::Cow;
use std::ffi::OsStr;
use std::io::stdout;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;

use anyhow::bail;
use anyhow::Context;
use anyhow::Result;


const GIT: &str = "git";


/// Format a git command with the given list of arguments as a string.
fn git_command<A>(args: &[A]) -> String
where
  A: AsRef<OsStr>,
{
  args.iter().fold(GIT.to_string(), |mut cmd, arg| {
    cmd += " ";
    cmd += &arg.as_ref().to_string_lossy();
    cmd
  })
}


/// Run git with the provided arguments and read the output it emits.
fn git_raw_output<A>(directory: &Path, args: &[A]) -> Result<Vec<u8>>
where
  A: AsRef<OsStr>,
{
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
fn git_output<A>(directory: &Path, args: &[A]) -> Result<String>
where
  A: AsRef<OsStr>,
{
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
fn git_run<A>(directory: &Path, args: &[A]) -> Result<bool>
where
  A: AsRef<OsStr>,
{
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
  let git_dir = git_raw_output(directory, &["rev-parse", "--absolute-git-dir"])?;
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


/// Ensure that git is usable and that `directory` points somewhere into
/// a valid git repository.
fn with_valid_git<W, F>(dir: &Path, writer: W, f: F) -> Result<Option<String>>
where
  W: Write,
  F: FnOnce(&Path, W) -> Result<Option<String>>,
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

  f(dir, w)
}


// TODO: Support reading information from .cargo_vcs_info.json.
fn revision_bare_impl<S, I, W>(dir: &Path, sources: S, writer: W) -> Result<Option<String>>
where
  S: IntoIterator<Item = I>,
  I: AsRef<Path>,
  W: Write,
{
  let mut w = writer;

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
/// The provided `directory` is a path expected to point somewhere into
/// the git repository in question. Typically, it can simply be set to
/// the value of the `CARGO_MANIFEST_DIR` variable, as set by Cargo.
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
  with_valid_git(directory.as_ref(), writer, |directory, writer| {
    let sources = [OsStr::new(""); 0];
    revision_impl(directory, sources.iter(), writer)
  })
}


/// Retrieve a git revision identifier that either includes the tag we
/// are on or the shortened SHA-1.
///
/// This function is meant to be run from a Cargo build script. It takes
/// care of printing necessary `rerun-if-changed` directives to the
/// provided writer. As a result, callers are advised to invoke it only
/// once and cache the result.
///
/// The provided `directory` is a path expected to point somewhere into
/// the git repository in question. Typically, it can simply be set to
/// the value of the `CARGO_MANIFEST_DIR` variable, as set by Cargo.
///
/// The function works on a best-effort basis: if git is not available
/// or no git repository is present, it will fail gracefully by
/// returning `Ok(None)`.
///
/// # Notes
/// Compared to [`git_revision_auto`], the revision identifier produced by
/// this function does not include any indication of local changes
/// (`+`).
pub fn git_revision_bare<D>(directory: D) -> Result<Option<String>>
where
  D: AsRef<Path>,
{
  with_valid_git(directory.as_ref(), stdout().lock(), |directory, writer| {
    // Because we don't care about local changes, we don't need to take
    // into consideration additional sources. All we care about are some
    // git files, and they are tracked automatically.
    let sources = [OsStr::new(""); 0];
    revision_bare_impl(directory, sources.iter(), writer)
  })
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
/// The provided `directory` is a path expected to point somewhere into
/// the git repository in question. Typically, it can simply be set to
/// the value of the `CARGO_MANIFEST_DIR` variable, as set by Cargo.
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
#[deprecated(note = "use git_revision_auto() function instead")]
pub fn git_revision<D, S, I>(directory: D, sources: S) -> Result<Option<String>>
where
  D: AsRef<Path>,
  S: IntoIterator<Item = I>,
  I: AsRef<Path>,
{
  with_valid_git(directory.as_ref(), stdout().lock(), |directory, writer| {
    revision_impl(directory, sources, writer)
  })
}


/// List all tracked objects.
fn list_tracked_objects(directory: &Path) -> Result<Vec<PathBuf>> {
  let top_level = git_raw_output(directory, &["rev-parse", "--show-toplevel"])?;
  let top_level = bytes_to_path(&top_level[..top_level.len() - 1])?;

  let args = &[
    OsStr::new("-C"),
    top_level.as_os_str(),
    OsStr::new("ls-files"),
    OsStr::new("--full-name"),
    OsStr::new("-z"),
  ];
  let output = git_raw_output(directory, args)?;
  let paths = output
    .split(|byte| *byte == b'\0')
    // The output may be terminated by a NUL byte and that will cause an
    // empty "object" to show up. We lack str's split_terminator, which
    // would cater to this case nicely, so we have to explicitly filter
    // that out.
    .filter(|object| !object.is_empty())
    .map(|object| Ok(top_level.join(bytes_to_path(object)?)))
    .collect::<Result<_>>()?;
  Ok(paths)
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
/// The provided `directory` is a path expected to point somewhere into
/// the git repository in question. Typically, it can simply be set to
/// the value of the `CARGO_MANIFEST_DIR` variable, as set by Cargo.
///
/// The function works on a best-effort basis: if git is not available
/// or no git repository is present, it will fail gracefully by
/// returning `Ok(None)`.
// TODO: Rename to `git_revision` once it has been removed with the next
//       breaking release.
pub fn git_revision_auto<D>(directory: D) -> Result<Option<String>>
where
  D: AsRef<Path>,
{
  with_valid_git(directory.as_ref(), stdout().lock(), |directory, writer| {
    let sources = list_tracked_objects(directory)?;
    revision_impl(directory, sources, writer)
  })
}
