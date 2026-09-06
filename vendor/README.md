# Android input correction

`winit-0.30.13` is the published winit crate, retained with its upstream license
and source provenance (`.cargo_vcs_info.json`, upstream commit
`e9809ef54b18499bb4f2cac945719ecc2a61061b`). Original crates.io archive SHA256:
`a6755fa58a9f8350bd1e472d4c3fcc25f824ec358933bba33306d0b63df5978d`.

Kokuban uses this local package only through the Android dependency alias
`winit_android`. Linux continues to use the registry version. The Android patch
is confined to `src/platform_impl/android/mod.rs` and its input helper: mouse
source, buttons, cursor/scroll events, and modifier state are translated into
the existing winit events before Kokuban handles them. Touch routing remains
in the upstream path.

The upstream Android backend treats pointer events as touch and does not emit
modifier changes. Its native input queue acknowledges the events before Java
view dispatch, so adding a Java mouse listener would not reliably repair the
missing event path. The correction remains in Rust and introduces no new
runtime library.

When updating winit, compare against the published crate, review whether the
upstream Android backend already contains equivalent support, and rerun the
Android controls/IME/lifecycle and desktop CI gates. Remove this local copy
when the supported upstream version covers these cases.
