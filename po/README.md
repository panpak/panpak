<!-- SPDX-License-Identifier: MIT OR Apache-2.0 -->
# Translations

The messages of Panpak that a user reads about permissions are translated with gettext catalogs. The message ids are the English sentences.

- `panpak.pot` is the template. It is written by the test `template_matches_the_messages` in `panpak-core/src/permissions.rs`:
  run `PANPAK_UPDATE_POT=1 cargo test -p panpak-core permissions` after a message changes.
- `<language>.po` is the catalog of one language (`it.po`, `de.po`, `pt_BR.po`). To add a language, copy `panpak.pot` to `<language>.po`, fill in the `msgstr` lines
  and set `Language:` in the header. No code changes: `build.rs` compiles every `.po` file in this folder into the program.
- A sentence without a translation is shown in English. A translation keeps the `{placeholders}` of its source (a test checks it). The plural rule understood is `n != 1`.
- The language is the first usable entry of `LANGUAGE`, `LC_ALL`, `LC_MESSAGES`, `LANG`; `C` and `POSIX` select English.
- Italian text is allowed in this folder only.
