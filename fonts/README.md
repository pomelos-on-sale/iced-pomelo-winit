# The default font

`Roboto-Subset.ttf` — Roboto Regular, cut down to the 99 codepoints this OS draws with:

```
U+0020-U+007E   ASCII
U+00B1  U+00D7  U+00F7   ± × ÷
U+200A          hair space
```

16 KiB on disk, and it goes into `.rodata` — flash, not RAM. That size is the point: iced's own
fallback is Fira Sans at **441 KiB**, and it is behind a feature precisely because nobody wants
that in a firmware image. For comparison, the full Roboto Regular this was cut from is 515 KiB.

It is what [`fonts::install_default`](../src/fonts.rs) installs when an app or a firmware has not
installed a font of its own, so that a program written for iced — which knows nothing about this
board's fonts — still draws text.

## How it was made

```bash
python3 -m fontTools.subset Roboto-Regular.ttf \
    --unicodes="U+0020-007E,U+00B1,U+00D7,U+00F7,U+200A" \
    --layout-features='' \
    --name-IDs=1,2,3,4,6,13,14 \
    --output-file=Roboto-Subset.ttf
```

(fontTools 4.65.0; `Roboto-Regular.ttf` is `assets/fonts/source/Roboto-Regular.ttf` in
`pomelo-os`.) The flags are not arbitrary:

* `--layout-features=''` drops GSUB/GPOS. With them, the subset keeps Roboto's kerning and
  ligature glyphs, which cost ~2 800 B of `glyf` for text this size; the sibling cut in
  `pomelo-os` has them off as well, so this reproduces it exactly — **same 100 glyphs, same
  `glyf` bytes**, i.e. the same pixels.
* `--name-IDs=1,2,3,4,6,13,14` keeps the family name (fontdb needs it: `install` reads the
  family off the face and makes it the sans-serif one) and the licence notice, which
  `pyftsubset` strips by default. That is the only difference from the older cut in `pomelo-os`,
  and it is the reason this file is 36 B larger: the asset carries its own licence.

## Licence

Roboto is licensed under the **Apache License 2.0** — see `LICENSE-APACHE-2.0.txt` next to this
file, and the notice inside the font itself (name table IDs 13 and 14, which the command above
preserves). Copyright 2011 Google Inc.
