// Copyright (C) 2022-2026 Daniel Mueller <deso@posteo.net>
// SPDX-License-Identifier: (Apache-2.0 OR MIT)

//! This library provides the means for including partly opinionated git
//! revision identifiers inside a Rust project (typically a binary). It
//! provides a set of functions, all meant to be invoked from a build
//! script, which inquire the current git revision being built against.
//!
//! Typical usage could look like this:
//! ```no_run
//! # use std::env;
//! use grev::git_revision;
//!
//! fn main() {
//!   let manifest_dir =
//!     env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR variable not set");
//!   let pkg_version = env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION variable not set");
//!
//!   let result = git_revision(manifest_dir).expect("failed to retrieve Git revision");
//!   if let Some(git_rev) = result {
//!     println!("cargo:rustc-env=VERSION={pkg_version} ({git_rev})");
//!   } else {
//!     println!("cargo:rustc-env=VERSION={pkg_version}");
//!   }
//! }
//! ```
//!
//! This logic, contained in a Cargo build script (typically `build.rs`,
//! located in a project's root), will cause the environment variable
//! `VERSION` to be set unconditionally when building the program. It
//! will contain the package version and, if available, the git revision
//! at which the build happened (including a modifier indicating if
//! local changes were present). If building at a git tag, the revision
//! string will include this tag. The main program would then inquire
//! the version string using `env!("VERSION")`.

use std::borrow::Cow;
use std::error::Error as StdError;
use std::ffi::OsStr;
use std::fmt::Debug;
use std::fmt::Display;
use std::fmt::Formatter;
use std::fmt::Result as FmtResult;
use std::io::stdout;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::result;


const GIT: &str = "git";


/// The error type used by the crate.
pub type Error = Box<dyn StdError + Send + Sync>;

type Result<T, E = ErrorInt> = result::Result<T, E>;


/// An [`Error`][StdError] implementation with user-friendly [`Debug`]
/// and [`Display`] impls.
///
/// We want this wrapper and not work with `Box<dyn StdError>`
/// everywhere, because a `String` can implicitly converted into the
/// latter, but we absolutely do not want its `Debug` impl to come into
/// play.
struct ErrorInt(Box<str>);

impl Display for ErrorInt {
  #[inline]
  fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
    Display::fmt(&self.0, f)
  }
}

impl Debug for ErrorInt {
  #[inline]
  fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
    Display::fmt(self, f)
  }
}

impl StdError for ErrorInt {}

impl<S> From<S> for ErrorInt
where
  S: Into<Box<str>>,
{
  #[inline]
  fn from(other: S) -> Self {
    ErrorInt(other.into())
  }
}


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
    .map_err(|err| format!("failed to run `{}`: {err}", git_command(args)))?;

  if !git.status.success() {
    let code = if let Some(code) = git.status.code() {
      format!(" ({code})")
    } else {
      String::new()
    };

    Err(ErrorInt::from(format!(
      "`{}` reported non-zero exit-status{}",
      git_command(args),
      code
    )))
  } else {
    Ok(git.stdout)
  }
}


/// Run git with the provided arguments and read the output it emits, as
/// a `String`.
fn git_output<A>(directory: &Path, args: &[A]) -> Result<String>
where
  A: AsRef<OsStr>,
{
  let output = git_raw_output(directory, args)?;
  let output = String::from_utf8(output).map_err(|err| {
    format!(
      "failed to read `{}` output as UTF-8 string: {err}",
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
    .map_err(|err| ErrorInt::from(format!("failed to run `{}`: {err}", git_command(args))))
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

  let path = from_utf8(bytes).map_err(|_err| {
    format!(
      "path `{}` contains non-UTF-8 characters",
      String::from_utf8_lossy(bytes)
    )
  })?;
  Ok(PathBuf::from(path).into())
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
    .map_err(|err| format!("failed to write Cargo rerun directive: {err}"))
  })?;
  let () = sources.into_iter().try_for_each(|path| {
    writeln!(
      writer,
      "cargo:rerun-if-changed={}",
      git_dir.join(path.as_ref()).display()
    )
    .map_err(|err| format!("failed to write Cargo rerun directive: {err}"))
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
      )
      .map_err(|err| format!("failed to write Cargo warning: {err}"))?;
      return Ok(None)
    },
    Err(err) => {
      writeln!(
        w,
        "cargo:warning=Failed to invoke `git`; unable to embed git revision: {err}"
      )
      .map_err(|err| format!("failed to write Cargo warning: {err}"))?;
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
/// Compared to [`git_revision`], the revision identifier produced by
/// this function does not include any indication of local changes
/// (`+`).
pub fn git_revision_bare<D>(directory: D) -> Result<Option<String>, Error>
where
  D: AsRef<Path>,
{
  with_valid_git(directory.as_ref(), stdout().lock(), |directory, writer| {
    // Because we don't care about local changes, we don't need to take
    // into consideration additional sources. All we care about are some
    // git files, and they are tracked automatically.
    revision_bare_impl::<[&OsStr; 0], &OsStr, _>(directory, [], writer)
  })
  .map_err(Error::from)
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
pub fn git_revision<D>(directory: D) -> Result<Option<String>, Error>
where
  D: AsRef<Path>,
{
  with_valid_git(directory.as_ref(), stdout().lock(), |directory, writer| {
    let sources = list_tracked_objects(directory)?;
    revision_impl(directory, sources, writer)
  })
  .map_err(Error::from)
}


#[cfg(test)]
mod tests {
  use super::*;


  fn _assert_send_sync() -> impl Send + Sync {
    ErrorInt::from("test")
  }


  /// Check various operations on our error types.
  #[test]
  fn all_things_errors() {
    let err = ErrorInt::from("foobar");
    assert_eq!(format!("{err}"), "foobar");
    assert_eq!(format!("{err:?}"), "foobar");

    let err = Error::from(err);
    assert_eq!(format!("{err}"), "foobar");
    assert_eq!(format!("{err:?}"), "foobar");
  }
}
