# Fonts

`Inter-Regular.ttf`, `Inter-SemiBold.ttf`, `Inter-MediumItalic.ttf` and
`Inter-BoldItalic.ttf` (italic text in text boxes, a weight heavier than the
upright it goes with) are from
[Inter 4.1](https://github.com/rsms/inter) by The Inter Project Authors,
licensed under the SIL Open Font License 1.1 (see `OFL.txt`).

They are modified: the glyphs mapped to the Unicode Private Use Areas
(U+E000–U+F8FF and planes 15–16) were removed, because the icon font
(Phosphor) uses those code points and Inter's own glyphs there would replace
the icons. Every other character, kerning and OpenType feature is unchanged.
Done with fontTools:

```sh
pyftsubset Inter-Regular.ttf --unicodes="U+0000-DFFF,U+F900-FFFF,U+10000-EFFFF" \
  --layout-features='*' --glyph-names --name-IDs='*' --name-languages='*' \
  --notdef-outline --hinting
```
