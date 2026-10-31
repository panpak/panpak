# Trademark policy

The Panpak **code** is free software under `MIT OR Apache-2.0`: you may use,
modify and redistribute it under those licenses. The **name** "Panpak" and the
project logo are not part of that grant. Section 6 of the Apache License 2.0
says so explicitly: the license does not give permission to use the trade
names, trademarks, service marks or product names of the licensor, except for
describing the origin of the work. The MIT license grants no trademark rights
either.

This page says in plain words what you may and may not do. It is a policy for
reasonable use, not a legal opinion; the project may refine it over time.

## You may

- Say that your program is **"compatible with Panpak"** or **"distributed in
  Panpak format"** (or that it "reads and writes the Panpak format").
- Say that you **use** or **build on** Panpak, and link to the project, when it
  is true.
- Refer to the project by name in documentation, articles, talks and packaging
  metadata to describe it accurately (for example "packaged with Panpak").
- Redistribute **unmodified** official releases under the name Panpak.
- Name a **separate** project in a way that makes clear it is independent
  (for example "X, a tool for Panpak packages").

## You may not

- Use "Panpak" (or a confusingly similar name or logo) as the name of **your
  own product, service, company or modified fork**, or as part of it, in a way
  that suggests the Panpak project made, approved or endorses it.
- Present a **modified** build as the official Panpak release.
- Use the logo without written permission.

## Forks that change the format

The name stands for a specific, specified format. A fork that **changes the
file format** (magic, footer, manifest, index, signing or verification rules)
in a way that is not compatible with `PANPAK-FORMAT.md` **must use a different
name**, and must not use the Panpak magic bytes for its own files.

Forks that keep the format unchanged and only differ in unrelated ways should
still make it clear in their name and description that they are not the
official project (for example "Foo, a fork of Panpak").

## Questions

Ask the maintainers through the project's issue tracker.
