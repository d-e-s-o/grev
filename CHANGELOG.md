Unreleased
----------
- Switched to using Rust 2021 Edition


0.1.3
-----
- Deprecated `git_revision` function in favor of new `git_revision_auto`


0.1.2
-----
- Deprecated `get_revision` function in favor of new `git_revision`
- Introduced `git_revision_bare` function for cases where a local
  modifications modifier is not desired


0.1.1
-----
- Fixed build on non-Unix systems
- Enabled CI pipeline comprising building and linting of the project
  - Added badge indicating pipeline status
- Switched to using edition 2018


0.1.0
-----
- Initial release
