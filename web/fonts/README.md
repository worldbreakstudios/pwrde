# Fonts embedded in the web build

gpui_web ships no system fonts — only IBM Plex Sans and Lilex — so the faces
pwrde's chrome and terminal grid expect are embedded here and registered at
boot (`web/main.rs`, `text_system().add_fonts`). cosmic-text falls back per
glyph across every registered face, so the symbol fonts only need to exist;
nothing names them.

- `JetBrainsMonoNerdFontMono-{Regular,Bold,Italic}.ttf` — "JetBrainsMono Nerd
  Font Mono", the family `renderer::FONT_FAMILY` names. JetBrains Mono
  (https://github.com/JetBrains/JetBrainsMono) patched with the Nerd Fonts
  glyph sets (https://github.com/ryanoasis/nerd-fonts). Bold-italic and the
  lighter weights are left out to keep the wasm bundle small.
- `NotoSansSymbols-Regular.ttf`, `NotoSansSymbols2-Regular.ttf`,
  `NotoSansMath-Regular.ttf` — Misc Technical (`⎿`, `⏺`), Dingbats (`✻`),
  arrows (`⇤`, the sidebar's collapse chip). From
  https://github.com/google/fonts/tree/main/ofl (the Symbols face is the
  `[wght]` variable font as published there).
- `NotoEmoji-Regular.ttf` — monochrome emoji (`✅`, `📡`), the `[wght]`
  variable font from the same place.
- `NotoSansJP-Fullwidth.ttf` — Noto Sans JP instanced at weight 400 and
  subset to U+FF01–FF5E (fullwidth forms) for the sidebar's `＋` chip; 12 KB
  instead of 9.6 MB. Regenerate with fonttools:
  `fonttools varLib.instancer NotoSansJP[wght].ttf wght=400 -o jp400.ttf &&
  pyftsubset jp400.ttf --unicodes=U+FF01-FF5E --no-hinting
  --output-file=NotoSansJP-Fullwidth.ttf`.

All of the above are under the SIL Open Font License 1.1
(https://openfontlicense.org).
