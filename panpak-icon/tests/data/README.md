Test icons for `panpak-icon`. `ok-<side>-*.png` must pass; `bad-*.png` must be refused (a text chunk, a colour profile, a
non-square image, an image under 16 pixels, and a GIF with a `.png` name). They were made with Pillow from a generated
Mandelbrot picture and a flat colour; no third-party image is in the repository. The checker is also tested with PNG files built byte
by byte in `src/lib.rs`.
