# Fonts embedded in the web build

gpui_web ships no system fonts — only IBM Plex Sans and Lilex — so the faces
pwrde's chrome and terminal grid expect are embedded here and registered at
boot (`web/main.rs`, `text_system().add_fonts`):

- `JetBrainsMonoNerdFontMono-{Regular,Bold,Italic}.ttf` — "JetBrainsMono Nerd
  Font Mono", the family `renderer::FONT_FAMILY` names. JetBrains Mono
  (https://github.com/JetBrains/JetBrainsMono) patched with the Nerd Fonts
  glyph sets (https://github.com/ryanoasis/nerd-fonts), both under the SIL
  Open Font License 1.1 — see https://openfontlicense.org. Bold-italic and the
  lighter weights are left out to keep the wasm bundle small; a style the
  renderer asks for and cannot find falls back to the nearest embedded one.
