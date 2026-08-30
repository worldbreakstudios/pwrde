# rcn Port Audit — canvas-painted surfaces and a port plan

Scope note. This audit was written against worktree `7871e227` on 2026-08-29. Two small
corrections to the brief came out of reading the code, and are reflected in the text
below rather than in CLAUDE.md (which was not modified): (1) the sidebar collapse is a
step function, not an animation — `App::sidebar_w()` returns `0.0` or
`sidebar_expanded_w` with no tween (src/main.rs:491-492), so "labels bleeding during
collapse" is a frame/state skew problem, not a mid-animation problem (§c, bug 1);
(2) main-window chrome labels *are* per-label content-masked
(src/main.rs:6007, 6059), so the bleed is not explained by a missing mask on the main
path; the surviving explanations are in §c. Everything else matched the brief.

## a. Canvas-painted surfaces

Every row below is a distinct surface painted on the canvas path. "Painted" means it is
built into a `Frame` by `Renderer::build_frame` (src/renderer.rs:503-1104) or
`Renderer::flyover_overlay` (src/renderer.rs:1794-2005) and rasterized by
`App::paint_terminal` (src/main.rs:~5590-6183), which also pushes each interactive
rect into `App::hot_rects`; hover/click then resolve by scanning that list in reverse
(z-order), not by element events (src/main.rs:3740-3747). Build order inside
`build_frame`: theme resolve (520-539) → `right_w` reserve, docked only (547-557, must
match `App::right_w_for`, comment 542-546) → `terminal_area` (558) → `layout_tiles`
(563-568) → `overlay_open` flag (578-584) → per-surface sections below.

| # | Surface | Built (src/renderer.rs) | Geometry (src/workspace.rs) | Hit-tested (src/main.rs) | State read | Coupling / notes |
|---|---------|--------------------------|------------------------------|--------------------------|------------|------------------|
| 1 | Window background gradient | Painted by `App::paint_terminal` *before* the frame quads (comment src/renderer.rs:592-593; paint src/main.rs:5893+) | none (full window) | n/a (not interactive) | `theme`, scheme from `term_theme::selected(theme::dark_active())` (520-526) | Tiled panes have no fills/shadows by design (738-745): "ground" *is* the gradient; divider gaps show it through (560-562). |
| 2 | Title bar strip (drag area between sidebar and traffic lights) | No fill; drag strip only, titlebar is transparent chrome (596-599) | `titlebar` (454-457), `TRAFFIC_LIGHT_SAFE_W` (461), `collapsed_drag_zone` (466-473) | `on_mouse_down` titlebar branch → `window.start_window_move` (src/main.rs:3188-3193; sidebar variant 3275-3280) | `sidebar_collapsed` | macOS traffic lights are *native* and float over the canvas — they own their clicks (comment src/main.rs:3276-3277). Any port must keep `collapsed_drag_zone` (78px × AREA_PAD+TILE_TAB_H) and `TRAFFIC_LIGHT_SAFE_W` semantics. |
| 3 | Sidebar on non-Sessions pages (Cleanup repos list, Settings sections + search box visuals) | Canvas paints **nothing** for these pages' rows — `sidebar_ui` element tree owns them (match arms src/renderer.rs:600-642; rows live in src/sidebar_ui.rs:414-529) | `tab_rect`/`tab_rect_at` (548-565), `settings_search_rect` (568-570 — literally `tab_rect(0)`), `sidebar_row_rect` (746-755) | **Still canvas**: `on_mouse_down` Settings branch resolves `settings_search_rect` → focus (src/main.rs:3291-3299) and section tabs via `workspace::tab_rect(i+1)` (3300-3307) | `page`, `settings_search_focus`, `section`, `sidebar_expanded_w` | Split-brain: visuals element-tree, hit-testing canvas. `settings_search_row` in the tree mirrors `settings_search_rect(1.0, sidebar_w())` (src/sidebar_ui.rs:489-529) so the unified hit-test keeps working. Bug-1 context (§c). |
| 4 | Collapsed-sidebar drag zone + collapse chip | Nothing painted while collapsed — `sidebar_w()==0` skips all sidebar painting (src/renderer.rs:596-598); expand affordances are part of the collapsed drag-zone hit | `collapsed_drag_zone` (466-473) | `on_mouse_down`: `sidebar_collapsed && collapsed_drag_zone.contains` (src/main.rs:3190); `sidebar_collapse_button` chip when open (3269-3272) | `sidebar_collapsed`, `sidebar_expanded_w` | Collapse toggle at src/main.rs:4462. Native traffic lights float over the zone's left end (comment src/workspace.rs:463-465). |
| 5 | Empty-state CTA + hint (Sessions page) | Pill + shadow + two labels, src/renderer.rs:604-636 ("New group" 616-627, "press ⇧⌘T" 626-635) | `empty_state_cta` (525-535), `empty_state_hint` (537-540) — centered in `terminal_area`, so they *move* when the sidebar width changes | `on_mouse_down` empty-state branch: only `empty_state_cta` opens the picker (src/main.rs:3237-3242) | `page`, `tool`, `hot` (hover, 610) | Positioned inside `terminal_area` → recomputed on sidebar/panel changes; painted on the dirty-gated canvas path while the sidebar tree re-composes every frame (§c bug 1). |
| 6 | Page-dot strip (sidebar bottom) | Dots are painted by the element tree (`page_dot_layer`, src/sidebar_ui.rs:355); canvas only pushes **hot rects** for the slots (src/renderer.rs:644-658, comment 649-651) | `page_strip` (1171-1174), `page_slot_rect` (1177-1189), `PAGE_STRIP_H`/`PAGE_SLOT`/`PAGE_SLOT_GAP` (1164-1167) | `on_mouse_down` → `page_slot_at` → `set_page` (src/main.rs:3284-3290) | `page`, `hot` | Visuals/interaction already split; hot rects on canvas path. |
| 7 | Tool ribbon (right-edge tool switcher) | Slot pills + per-tool `ribbon_icon` glyphs, active/hover pill, src/renderer.rs:662-685 | `ribbon` (1736-1744), `ribbon_slot_rect` (1747-1756), `RIBBON_W` (1622) | `on_mouse_down` ribbon slots → `toggle_tool` (src/main.rs:3214-3222); hover via hot rects | `tool`, `chrome.ribbon_tools`, `hot` | `RIBBON_W` is subtracted from `terminal_area` *only when a tool is docked* (547-557) — ribbon itself always visible. |
| 8 | Tool panel — Launch placeholder card ("… view coming soon") | The only tool still painted on canvas: comment src/renderer.rs:686-688 ("Launch has no element overlay yet"); card 694-696, title labels 697-702, placeholder line 703-710 | `tool_panel` (1760-1772) — *floating* branch overlaps `terminal_area`; width only reserved when docked | `on_mouse_down` panel branch swallows clicks ("Placeholder body: nothing interactive yet", src/main.rs:3209-3212); left-edge grab → `Drag::ToolPanelResize` (3203-3208); floating blur-dismiss (3223-3231) | `tool`, `tool_panel_floating`, `tool_panel_w` | PR/Local-diff panels are already element trees (`pr_ui::tool_panel_overlay`, src/pr_ui.rs:38-51; src/local_diff_ui.rs:185-190). Bug-2 (§c). |
| 9 | Tool panel — panel resize grip | `push_resize_grip` line at panel edge (src/renderer.rs:1011-1029, fn 2146-2180) | `TOOL_PANEL_RESIZE_GRAB` (1628) | `Drag::ToolPanelResize` (src/main.rs:3203-3208, 3537+ drag machine) | `resize_hover`, `tool_panel_w` | Grip visual mirrors canvas-resolved `ResizeHover`. |
| 10 | Tile cards (pane backgrounds, focused divider hairline) | No fills/shadows for panes by design (src/renderer.rs:738-745); focused tile painted *last* (728-736); focused divider hairline 760-764 | `layout_tiles`/`walk`/`split_rects` (1259-1514), `rect_at_path` (1517-1534) | indirectly — divider branches below; tile body itself lets clicks fall through to terminal | tile tree, `focus`, collapse anims | **Keep for PTY sizing**: `layout_tiles`, `tile_content` (1542-1545), `rect_at_path` are the grid geometry source. |
| 11 | Tile tab strips (active pill, hover pill, unread dot, tab labels) | src/renderer.rs:755-799 (strip + active/focused pills 766-799), whole-card side-strip hover 800-807, per-tab pills 899-917, labels 954-971, unread dot 942-953 | `tile_tab_bar` (1537-1539), `tile_tab_rect` (1561-1567), `TILE_TAB_H` (436), `TILE_TAB_MAX_W` (446), `tab_strip_rect` (481-487 — collapsed top-left tile cedes strip left end to traffic lights; *all* strip consumers must feed it) | tab strip drag-arms in sidebar branch tail of `on_mouse_down` (~3308-3536); hover via hot rects | tabs, focus, `collapse_anim`, `hot` | `tab_strip_rect` + `TRAFFIC_LIGHT_SAFE_W` must survive any port. |
| 12 | Tile tab close buttons ("×" chip) | src/renderer.rs:918-933 chip, "×" label 972-983, hot 936-937 | `tile_tab_close_rect` (1592-1602) | part of tab strip branches of `on_mouse_down` (~3308+); right-click menu src/main.rs:1473 | tabs, `hot` | Mirrors `section_delete_rect` proportions (816-825). |
| 13 | Tile caret chevron + collapsed unread badge | src/renderer.rs:820-839 (`CaretSpec` angle from `collapse_anim`), badge 843-853 | `tile_caret_rect` (1550-1554, only when parent split) | via tile hot rects / tab strip branches | collapse anim, unread counts | Pure decoration + collapsed affordance. |
| 14 | Drop hints during tab drag (insert gap line) | src/renderer.rs:988-991 translucent accent overlay on the hovered tab | `tile_tab_insert_gap` (1572-1578), `tile_tab_insert_line` (1581-1586) | computed in `on_mouse_move` tab-drag state machine (src/main.rs:3537-3750); drop commit in `on_mouse_up` (~3752-3832) | `drag`, hovered tab | Drop target = half-tab boundary; keep math when tabs move to an element tree. |
| 15 | Terminal grid — text runs, cursor, links, block glyphs, selection | `snapshot_pane` src/renderer.rs:2010-2145 invoked per pane at 859-881; grid text actually shaped in `App::paint_terminal` src/main.rs:5883-6076 with `renderer::FONT_FAMILY` at cell metrics; cursor drawn only when focused && no overlay (862-867); link hover 868-870; `selection_rects` (881, fn renderer.rs:449) | `terminal_area` (1202-1213), `tile_content` (1542-1545), `content_origin`/`cell_at`/`grid_size_for` (renderer.rs:422-446), `rect.rs` char quads | mouse→cell via `renderer.cell_at` + `session.link_at` (`on_mouse_move` src/main.rs:3717-3739); selection drag in `on_mouse_move`/`on_mouse_up` | session grid, `focus`, `overlay_open` (cursor suppressed 585-587), `link_hover` | **The one surface that stays canvas-painted** per the goal; it must be hosted inside an rcn container instead. PTY resize consumes `tile_content` rects. |
| 16 | Resize grips/dividers (sidebar edge, tile dividers) | Divider grips src/renderer.rs:1004-1010 via `push_resize_grip`; sidebar grip was *moved off canvas* to `resize_grip_layer` in the element tree (comment 998-1002 — the element tree swallowed the canvas's inner half) | `Divider{path,rect,dir}` (1215-1219), `resize_hover_at` (1234-1256), `rect_at_path` (1517-1534) | `on_mouse_down` divider branch `dividers.find(rect.inflate(grab))` → `Drag::Divider` (src/main.rs:3244-3255); commit in `on_mouse_up` via `rect_at_path` ratio | `resize_hover`, split ratios | Divider drag *commits* a new ratio into the tree — keep `rect_at_path` + `resize_hover_at`. |
| 17 | Directory picker overlay | `picker_overlay` src/renderer.rs:1106-1236, painted into the separate `picker_quads/picker_labels` layer; 0.30 scrim 1119-1122 | `PickerLayout::compute` (src/picker.rs) | `on_mouse_down` modal chain → `overlay_click` (src/main.rs:3090-3102 → fn ~2781-3010); hot rects cleared when `overlay_open` (renderer.rs:1057-1059) | `picker`, `overlay_open` | Part of the canvas modal system; scrim alpha `th.scrim @ 0.30` is the modality invariant. |
| 18 | Fork picker overlay | `fork_overlay` src/renderer.rs:1314+ (scrim 1326-1328) | fork layout consts in src/picker.rs | same modal chain as #17 | `fork` state | Same layer. |
| 19 | Profile picker overlay | `profile_overlay` src/renderer.rs:1413+ (scrim 1427-1428) | profile layout in src/picker.rs | same modal chain | `profile` state | Same layer. |
| 20 | Save-workspace modal | `save_overlay` src/renderer.rs:1507+ (`save_layout`), scrim 1554-1555 | save layout consts | same modal chain | `save` state | Same layer. |
| 21 | Command palette | `palette_overlay` src/renderer.rs:1226+ (model in src/palette.rs), scrim 1240-1241 | palette layout consts | same modal chain; keyboard actions through gpui keystrokes, not canvas `on_key_down` | palette query/selection | Same layer. |
| 22 | Confirm dialog + scrim | `confirm_overlay` src/renderer.rs:1685+ (scrim 1699+) | confirm layout consts | first in modal chain (src/main.rs:3090-3102) | `confirm` | Gates the Cleanup element tree off while open (`render_cleanup` shown only when `confirm.is_none()`, src/main.rs:5643-5645) — the canvas scrim owns the whole screen. |
| 23 | Toast / message banner | `message_overlay` src/renderer.rs:1748+ | message layout consts | same modal chain | `message` | Same layer. |
| 24 | Flyover — tab bar (tabs, close, unread dots, labels) | `flyover_overlay` src/renderer.rs:1794-1938 (card bg + `Shadow::Card` 1835, top border 1837-1843); in-window layer painted at src/main.rs:5830 gated only on `flyover_anim > 0 && !flyover_windowed`; popout paints unconditionally (src/main.rs:6502) | `flyover_rect` (1639-1659), `flyover_tab_bar` (1662-1664), `flyover_tab_rect` (1683-1699 — cedes `TRAFFIC_LIGHT_SAFE_W` when maximized), `flyover_tab_close_rect` (1703-1719), `flyover_buttons_w` (1674-1676) | popout: `FlyoverPopout::on_mouse_down` src/main.rs:6343-6384; in-window: flyover branch 3110-3160 (tab index 3136+, × close) | `flyover_active`, tabs, `flyover_anim`, `flyover_windowed` | Second window (`FlyoverPopout`, src/main.rs:6260-6281) paints from the *shared* App entity with its own Renderer ("The sessions never move — only which surface paints them", 6255-6259); labels clipped via `with_content_mask` in `paint_flyover_layer` (6242-6251). |
| 25 | Flyover — minimize / maximize buttons ("–" / "□") | src/renderer.rs:1940-1977; hit-test src/main.rs:3126-3133 (min → `toggle_flyover`, max → `flyover_toggle_maximized`) | `flyover_minimize_rect` (1722-1725), `flyover_maximize_rect` (1728-1731); `flyover_buttons_w` reserves two bar-heights at the bar's right end, shaping *every* flyover tab width | as #24 | `flyover_anim`, `flyover_windowed`, `overlay_open` | Paint is **not** gated on `overlay_open` while hot rects are (5855-5857) — bug 3 (§c). |
| 26 | Flyover — resize grab (top edge) + content | Top-edge grab → `Drag::FlyoverResize` unless maximized (src/main.rs:3115-3120, `FLYOVER_RESIZE_GRAB` 1619); content via `snapshot_pane` src/renderer.rs:1989-2002 | `flyover_content` (1667-1670), `FLYOVER_INSET` (1607), `MIN/MAX_FRAC` (1614-1615) | popout scroll/forward: src/main.rs:6428-6458; `forward_popout_mouse` maps cursor→cell (6305-6316) | `flyover_frac`, session scroll | Popout mouse reporting duplicates the app-side scroll/report logic — keep parity if the surface moves. |
| 27 | Debug frame stats overlay | src/renderer.rs:1035-1052 | none | none | `debug.overlay` setting | Dev-only; harmless to keep or drop. |

Nothing else paints on the canvas path: reading `build_frame` end to end, sections
592-1052 are enumerated above, 1057-1059 is the overlay modality guard, 1062-1087 are
the modal builders (#17-23), 1094-1104 assemble the `Frame` fields
(`quads/text/fg_quads/labels/hot` + `flyover_*` + `picker_*` layers), and
1794-2005 is the flyover layer (#24-26). `ribbon_icon` (2181-2300) and `glass`/`pill`/
`px_rect` (2301-2345) are paint helpers for #7 and the pills, not separate surfaces.

## b. Element-tree surfaces (already ported)

All of these are absolutely-positioned rcn element trees composed as siblings *over*
the full-window canvas in `App::render` (canvas child src/main.rs:5621-5633; overlays
5639-5674). The canvas underneath still resolves every click and drag during the
migration (comment src/main.rs:5634-5638) — that residual coupling is the "remaining
canvas coupling" column. The established positioning pattern is **inset mirroring**:
an overlay mirrors `workspace::terminal_area` as logical insets (`AREA_PAD`,
`sidebar_w`, `right_w`) re-resolved in gpui layout every frame, never as surface-size
math (src/cleanup_ui.rs:31-38).

| Surface | Entry point | How positioned (which workspace rect it mirrors) | Remaining canvas coupling |
|---|---|---|---|
| Sessions sidebar | `render_sidebar` src/sidebar_ui.rs:134-184, composed at src/main.rs:5639 | `.absolute().left/top(0).w(sidebar_w())` + `.overflow_hidden()` (155-163); height clipped by `flyover_ceiling()` (164-167, fn 197 — tracks `App::flyover_rect_now` per frame so the canvas flyover can slide over it) | rows/pinned/groups still hit-tested in main.rs sidebar branch (src/main.rs:3258-3307, rows ~3308+); `resize_grip_layer` (src/sidebar_ui.rs:267-298) only *mirrors* the canvas-resolved `ResizeHover::Sidebar`; page-dot hot rects pushed on canvas (src/renderer.rs:652-658) and resolved by `page_slot_at` (src/main.rs:3284-3290); settings search row mirrors `workspace::settings_search_rect` (src/sidebar_ui.rs:489-529) so canvas hit-test keeps working; a11y via `font_scale()` = `workspace::row_font_scale()` (113-115) |
| Cleanup page | `render_cleanup` src/cleanup_ui.rs:27+, composed at src/main.rs:5643-5645 (gated `Page::Cleanup && confirm.is_none()` — canvas scrim owns the screen while a confirm is open) | inset-mirrors `terminal_area`: pad `AREA_PAD`, left `sidebar==0 ? pad : sidebar`, right `right_w()+pad` (src/cleanup_ui.rs:35-38) | clicks on its list land on rcn elements, but canvas hover/dividers still run underneath; keyboard via gpui actions |
| Notes page | `render_notes` src/notes_ui.rs:44-88, composed at src/main.rs:5652-5657 | same inset-mirror | vault/file lists live in the sidebar tree (shared sidebar state); keys via actions |
| Settings page | `render_settings` src/settings_ui.rs:41-44, composed at src/main.rs:5660-5663 | same inset-mirror; bg `on_mouse_down` drops recording + search focus (src/settings_ui.rs:85-93) | **search box + section tabs still hit-tested on canvas** (src/main.rs:3291-3307); `settings_search_focus` mutated in main.rs key handling |
| PR tool panel | `pr_ui::tool_panel_overlay` src/pr_ui.rs:38-51 (shared chrome fn), composed via `render_pr` at src/main.rs:5667-5670, gated `visible_tool() && !modal_overlay_open()` | mirrors `workspace::tool_panel` from `AREA_PAD`/`RIBBON_W`/`TOOL_PANEL_FLOAT_INSET`, with floating vs docked branches — i.e. it reproduces the same float-over-terminal geometry policy | Float/Dock toggle is an rcn button (src/pr_ui.rs:441-445) but panel resize + blur-dismiss still resolved on canvas (src/main.rs:3203-3231) |
| Local-diff tool panel | src/local_diff_ui.rs:185-190 delegates to `tool_panel_overlay`, composed at src/main.rs:5671-5674 | same as PR panel; shares `DiffViewState`/`build_diff_render` with pr_ui | same as PR panel |

Shared infra: 17 vendored rcn components in src/ui/ (assets badge button button_group
card checkbox hover_card input kbd label motion select separator skeleton switch table
theme), managed by rcn.toml; `Theme::from_chrome` set at every overlay entry
(e.g. src/sidebar_ui.rs:135, src/cleanup_ui.rs:29). The rcn registry also offers, not
yet vendored: accordion alert alert-dialog avatar breadcrumb collapsible combobox
command context-menu dialog drawer dropdown-menu empty field item menubar popover
progress resizable scroll-area sheet sidebar slider spinner tabs textarea toast toggle
toggle-group tooltip.

## c. Likely causes of the reported bugs

**Bug 1 — canvas-painted labels bleeding outside a collapsed sidebar.**
`App::sidebar_w()` is a step function: `if self.sidebar_collapsed { 0.0 } else { self.sidebar_expanded_w }` (src/main.rs:491-492); there is no sidebar animation. Both
paths skip painting when collapsed (canvas: src/renderer.rs:596-598; tree:
src/sidebar_ui.rs:137-138), and main-window chrome labels *are* per-label
content-masked (src/main.rs:6007, 6059) — so the bleed is a **frame/state skew**:
sidebar visuals live on two paths with different staleness. The canvas path is
dirty-gated (`self.dirty = false` at the end of `paint_terminal`, src/main.rs:6065)
while the element tree re-composes every render. Canvas visuals that still belong to
the sidebar — the Sessions empty-state CTA+hint (src/renderer.rs:604-636), which is
positioned inside `terminal_area` and therefore *widens* when the sidebar collapses
(src/workspace.rs:525-540), and the page-slot hot rects (src/renderer.rs:652-658) — can
survive one state-change staler than the tree; and the invisible-but-live
`settings_search_rect` hit (src/main.rs:3291-3299; rect == `tab_rect(0)`,
src/workspace.rs:568-570) outlives its visible row. This split is the same failure
class already hit once: the canvas sidebar grip had to be moved into the tree because
the tree "swallowed" its inner half (src/renderer.rs:998-1002). *Does porting fix it?*
Yes for the class: moving the remaining canvas sidebar paints/hits (empty state, page
slots, settings search) into `sidebar_ui` puts every sidebar pixel on the
self-clipping, always-fresh element path (`overflow_hidden` + `flyover_ceiling`,
src/sidebar_ui.rs:155-167), eliminating stale-canvas frames entirely.

**Bug 2 — "Launch view coming soon" tool-panel placeholder overlapping terminal content.**
Fully verified chain: the comment at src/renderer.rs:686-688 states Launch keeps the
canvas placeholder because it "has no element overlay yet"; the card + labels are drawn
at src/renderer.rs:694-710 from `workspace::tool_panel` (693). The floating panel
geometry (src/workspace.rs:1760-1772) insets it only `TOOL_PANEL_FLOAT_INSET` from the
window's right edge, and `right_w` reserves width *only when docked* (src/renderer.rs:547-557),
so a floating Launch panel sits on top of terminal content. The hit-test twin swallows
clicks over it (src/main.rs:3209-3212). *Does porting fix it?* Porting Launch to a
`pr_ui::tool_panel_overlay`-style tree (src/pr_ui.rs:38-51) removes the canvas-drawn
card, but the overlap itself is geometry policy (float-over-content, no width
reserved) that PR/Local-diff already live with — the port step must explicitly choose
dock-reserve vs float-overlap.

**Bug 3 — stray minimize/maximize pair at bottom-right while a picker modal is open.**
The flyover layer paints its own "–"/"□" window controls at src/renderer.rs:1940-1977
(rects: src/workspace.rs:1722-1731). The in-window layer is gated only on
`self.flyover_anim > 0.0 && !self.flyover_windowed` (src/main.rs:5830) — **not** on
`overlay_open` — while the modal system only clears *hot rects* when an overlay is open
(src/renderer.rs:1057-1059; append suppressed at src/main.rs:5855-5857) and nulls the
cursor. The picker's 0.30-alpha scrim (src/renderer.rs:1119-1122; painted over at
src/main.rs:6035-6062) dims but does not hide them, and the modal branch of
`on_mouse_down` (3090-3102) eats the click before the flyover branch (3126-3133) — so
the buttons are visible-but-inert. The popout window paints the same layer
unconditionally when visible (src/main.rs:6502). *Does porting fix it?* A minimal fix
is gating the layer on `!overlay_open` exactly where hot rects are (5855-5857); porting
the flyover to an element tree under the same scrim/modality as the other overlays
removes the cause structurally.

## d. Port plan

Ordered, smallest and most isolated first; the terminal grid is hosted last. Two
patterns from the already-ported surfaces are reused throughout: **inset mirroring**
(`render_cleanup`, src/cleanup_ui.rs:31-38) and **state-keyed mirror layers** for
canvas-resolved interactions during migration (`resize_grip_layer`, src/sidebar_ui.rs:267-298).
After every step the canvas keeps resolving clicks until the step's handler move is
included — never both off in one PR. rcn components marked "registry" need `rcn add`
(into src/ui/ per rcn.toml; never `rcn init`); everything else is vendored already.

1. **Gate the flyover chrome on overlay modality (bug-3 stopgap).**
   Surfaces: flyover min/max buttons (#25). Change: gate the in-window layer and the
   popout paint on `!overlay_open`, mirroring the hot-rect suppression (src/main.rs:5855-5857).
   rcn: none. Geometry: unchanged. Handlers: unchanged. Risk: low (a `where` clause on
   one paint call site + one in `FlyoverPopout::paint`). Acceptance: open the dir
   picker while the flyover is visible — no "–"/"□" at bottom-right; hot rects and
   clicks unchanged.

2. **Empty-state CTA + hint → element tree.**
   Surfaces: #5. rcn: vendored `button`, `label`; registry `empty` if desired (not
   required). Position: `div().absolute()` mirroring `workspace::empty_state_cta` /
   `empty_state_hint` insets (src/workspace.rs:525-540) inside the existing
   render_sidebar sibling block. Geometry removed: `empty_state_cta`/`empty_state_hint`
   (525-540) once hot rects stop needing them. Handlers moved: `on_mouse_down`
   empty-state branch (src/main.rs:3237-3242) → element `on_click`; hover pill via
   element hover. Geometry kept: `terminal_area` (it centers the CTA). Risk: low.
   Acceptance: ⌘-click "New group" opens the dir picker; CTA never paints when
   `sidebar_w()==0`; bug-1 class loses its main canvas source.

3. **Page-dot strip fully into the sidebar tree.**
   Surfaces: #6 (already half-ported). rcn: registry `tooltip` for page names (optional).
   Change: delete the canvas hot-rect push (src/renderer.rs:652-658); `page_dot_layer`
   (src/sidebar_ui.rs:355) gets real `on_click` → `set_page` per slot; move `page_slot_at`
   resolution into the layer. Geometry removed: `page_strip`/`page_slot_rect` from the
   canvas path (keep pure fn for tests). Handlers moved: src/main.rs:3284-3290. Risk: low.
   Acceptance: page switching works with the canvas sidebar branch's page code deleted;
   no dead hot rects when collapsed.

4. **Settings sidebar search box + section tabs into `settings_ui`.**
   Surfaces: #3 (the hit-test half; visuals already tree-side). rcn: vendored `input`
   for the search row (replacing the mirror-only row at src/sidebar_ui.rs:489-529).
   Geometry removed: `settings_search_rect` (src/workspace.rs:568-570) and the
   `tab_rect(i+1)` section hit-tests (src/main.rs:3291-3307). Handlers moved: search
   focus + section switching into element handlers + existing key actions. Keep:
   `tab_rect_at` while any other consumer exists. Risk: low-medium (focus/IME timing).
   Acceptance: typing in the Settings search works with zero `settings_search_rect`
   references left in src/main.rs; ⌘F focuses it.

5. **Confirm dialog + scrim → element tree (first modal).**
   Surfaces: #22. rcn: registry `alert-dialog` (or `dialog`), vendored `button` for
   actions. This establishes the modal pattern the other four modals copy: element
   scrim + dialog *above* every canvas layer and *above* the flyover layer, replacing
   the paint-order scrim (src/renderer.rs:1699+). Keep `render_cleanup`'s
   `confirm.is_none()` gate inverted into the tree owning modality
   (src/main.rs:5643-5645). Handlers moved: confirm arm of `overlay_click`
   (src/main.rs:~2781-3010) + the confirm-first branch of `on_mouse_down` (3090-3102).
   Geometry removed: `confirm_overlay` builder (src/renderer.rs:1685+). Risk: medium
   (modality ordering). Acceptance: confirm opens over tiles, sidebar, *and* flyover;
   Esc/⌘. buttons behave; no canvas confirm code remains.

6. **Message/toast + save modal → element trees.**
   Surfaces: #23, #20. rcn: registry `toast` (message) and `dialog` (save), vendored
   `input`/`button`/`table` for save rows. Handlers moved: corresponding `overlay_click`
   arms + keyboard arms. Geometry removed: `message_overlay` (src/renderer.rs:1748+),
   `save_overlay`/`save_layout` (1507+/1514). Risk: medium. Acceptance: save-workspace
   round-trips (persist + reload), toast auto-dismisses, both sit above flyover.

7. **Picker/fork/profile trio → element trees.**
   Surfaces: #17, #18, #19. rcn: registry `command` (palette-like list) or
   `popover` + vendored `input`, `kbd` for hints. Handlers moved: three `overlay_click`
   arms; `PickerLayout::compute` geometry (src/picker.rs) replaced by flex layout with
   a kept `max height` constant. Geometry removed: `picker_overlay`, `fork_overlay`,
   `profile_overlay` builders (src/renderer.rs:1106+, 1314+, 1413+); the
   `picker_quads/picker_labels` third layer (1094-1097) can be deleted once empty.
   Risk: medium (picker is the highest-traffic modal). Acceptance: ⌘T dir picker, fork
   picker, profile picker all keyboard-navigable; frame budget unchanged; `picker_*`
   fields gone from `Frame`.

8. **Command palette → element tree.**
   Surfaces: #21. rcn: registry `command` (+ `dialog`). Handlers moved: palette
   `overlay_click` arm; keys already action-based. Geometry removed:
   `palette_overlay` (src/renderer.rs:1226+). Risk: low-medium. Acceptance: ⌘K palette
   opens over everything, fuzzy filter latency unchanged, no palette code in build_frame.

9. **Launch tool panel → element tree (bug-2).**
   Surfaces: #8 (and #9's panel grip stays canvas until step 12). rcn: registry `empty`
   (placeholder body), vendored `card`/`button`; reuse `pr_ui::tool_panel_overlay`
   chrome (src/pr_ui.rs:38-51). Change: delete the canvas card (src/renderer.rs:686-711)
   and the click swallow (src/main.rs:3209-3212); **decide the geometry policy** — keep
   float-over-content for parity, or reserve width via `right_w_for` like docked
   panels (src/renderer.rs:547-557). Keep: `tool_panel` (resize target). Risk: medium
   (visible layout policy decision). Acceptance: selecting Launch in the ribbon shows
   the panel as an element tree; no "coming soon" pixels on the canvas; tiles never
   intersect it if dock-reserve chosen.

10. **Tile tabs (tab strips, close, caret, drop hints) → element tree.**
    Surfaces: #11, #12, #13, #14. rcn: registry `tabs` + `tooltip`, vendored `button_group`
    for close chips. Positioning: an overlay per tile mirroring `tile_tab_bar` insets
    (src/workspace.rs:1537-1539). Keep: `tile_tab_rect`/`tile_tab_close_rect`/
    `tile_tab_insert_gap`/`tile_tab_insert_line` *inside the element layer* — the drag
    state machine (src/main.rs:3537-3750 move, 3752-3832 up) still needs gap/line math
    for drop hints until this step replaces it with element drop targets. Geometry
    removed from canvas: tab pills/labels/close/dot/hint painters (src/renderer.rs:755-991
    minus content). Handlers moved: tab drag-arm + right-click menu (src/main.rs:1473).
    Keep: `tab_strip_rect` + `TRAFFIC_LIGHT_SAFE_W` for the collapsed top-left tile.
    Risk: **high** (drag-and-drop reordering is the most stateful interaction).
    Acceptance: drag a tab between tiles — insert line renders, drop reorders, close
    and unread dots correct, no tab pixels left in build_frame.

11. **Flyover → element tree (in-window layer first).**
    Surfaces: #24, #25, #26 (in-window). rcn: vendored `button` (min/max/close),
    registry `resizable` for the top-edge grab + frac persistence, `sheet` as the
    container idiom. Positioning: absolute overlay mirroring `flyover_rect`'s
    lerp/maximized math — keep `flyover_rect`/`flyover_content` as the *source of
    truth* the element reads per frame (like `flyover_ceiling`, src/sidebar_ui.rs:197).
    Handlers moved: in-window flyover branch of `on_mouse_down` (src/main.rs:3110-3160)
    → element handlers; `Drag::FlyoverResize` onto the `resizable` handle. Geometry
    removed: `flyover_overlay` painter (src/renderer.rs:1794-2005);
    `flyover_buttons_w` becomes a layout constant of the tree (must preserve its
    two-bar-height reservation so tab widths don't shift). Keep: `flyover_content`
    rect — the popout (step 12) and PTY sizing still consume it. Risk: high.
    Acceptance: flyover slides in/out, maximizes clearing traffic lights
    (src/workspace.rs:1885 test), tabs/close/min-max all element-handled; popout
    unchanged and still pixel-identical.

12. **Resize grips + tool-panel resize → element handles.**
    Surfaces: #9, #16 (+ panel blur-dismiss). rcn: registry `resizable` (handles),
    vendored `separator` for hairlines. Change: sidebar grip already mirrored
    (src/sidebar_ui.rs:267-298) — flip it to a real handle that *sets* `ResizeHover`;
    divider grips become element handles that drive the same
    `rect_at_path`-ratio commit (src/main.rs:3244-3255, up-commit ~3752-3832). Keep:
    `resize_hover_at` (unchanged semantics for keyboard/edge cases), `rect_at_path`.
    Geometry removed: `push_resize_grip` painters (src/renderer.rs:1004-1029, 2146-2180).
    Risk: medium. Acceptance: drag each divider type — ratio commits identically;
    `resize_hover_at` tests (src/workspace.rs:2207) still pass.

13. **Host the terminal grid inside an rcn container (last).**
    Surfaces: #10 (background/hairlines only) + #15 + #1. Change: replace the
    raw-canvas root in `App::render` with an rcn/gpui element-tree container (registry
    `scroll-area` semantics live in the terminal's own session scroll, so the container
    is a plain absolute `div` stack) whose *child* remains the existing
    `canvas`-painted terminal grid (src/main.rs:5621-5633), sized from
    `terminal_area`/`tile_content` (src/workspace.rs:1202-1213, 1542-1545). The grid
    keeps: cell metrics, `cell_at`, selection, links, PTY resize consumer
    (`right_w_for`, src/renderer.rs:542-546), and `snapshot_pane` for the flyover.
    Handlers *kept* on canvas: terminal clicks/typing/scroll/selection/reporting —
    only the chrome around it has moved by now. Geometry removed: none (this step
    removes the *window gradient* + tile hairline painting only if trivially
    expressible as element `bg`s; otherwise leave them). Risk: **highest** — do not
    move any terminal interaction here. Acceptance: PTY grid pixel-identical at both
    scale factors; `renderer.surface_size` sync (src/main.rs:5683-5687) unchanged;
    resize a tile → PTY cols/rows identical to pre-port.

14. **Cleanup: delete dead geometry + hot-rect machinery.** After 2-12: remove
    `sidebar_row_rect`/`pinned_*`/`page_*`/`settings_search_rect`/`flyover_tab_*`/
    `flyover_minimize_rect`/`flyover_maximize_rect` consumers that no longer exist
    (src/workspace.rs inventory in §a), then shrink `App::hot_rects` to the terminal
    grid's needs only. Risk: low. Acceptance: `cargo build` clean; visual diff
    screenshot sweep against pre-port baseline.

Rough size check: steps 1-4 are each well under 300 lines; 5-8 are 300-600 each;
9-12 are 400-900 each; step 13 is deliberately the only one touching the paint root
(~600). Each is one PR.

## e. Constraints to preserve

- **Chrome theme colors**: every surface must keep resolving through
  `Theme::from_chrome` (set_global at every overlay entry, e.g. src/sidebar_ui.rs:135,
  src/cleanup_ui.rs:29); scheme via `term_theme::selected(theme::dark_active())`
  (src/renderer.rs:526); `th.scrim` at 0.30 alpha for every modal (src/renderer.rs:1122);
  `ON_ACCENT_INK` (src/renderer.rs:44) for focused-active tab labels; `Shadow::Card`
  on the flyover card (src/renderer.rs:1835).
- **Fonts**: terminal grid in `renderer::FONT_FAMILY` = "JetBrainsMono Nerd Font Mono"
  (src/renderer.rs:37) at cell metrics (src/main.rs:5883-6076); chrome text at
  `chrome_font_size` scaled by `chrome_font_scale()` (src/renderer.rs:317-319) —
  element trees must multiply through it (the existing overlays do, src/sidebar_ui.rs:113-120).
- **Spacing constants**: `TITLEBAR_H` (src/workspace.rs:369), `TILE_TAB_H` (436),
  `CARD_H` (375), `TILE_GAP`/`AREA_PAD` (439/445), `TAB_GAP` (426), `SIDEBAR_PAD` (428),
  `TILE_TAB_MAX_W` (446), `PAGE_STRIP_H` (1164), `FLYOVER_INSET` (1607),
  `TOOL_PANEL_FLOAT_INSET` (1630), `RIBBON_W` (1622) — pixel-identical means these
  constants, not recomputed values, feed the new layouts.
- **Traffic-light safe area**: `TRAFFIC_LIGHT_SAFE_W` (src/workspace.rs:461),
  `collapsed_drag_zone` (466-473), and `tab_strip_rect`'s collapsed-top-left ceding
  (481-487) exist because macOS traffic lights are native windows floating over the
  canvas (comments 463-465, src/main.rs:3276-3277). Every ported surface must keep
  ceding that strip; maximized flyover tabs already do (src/workspace.rs:1683-1699).
- **PTY resize coupling**: `right_w` reserve "must match App::right_w_for so painting,
  PTY sizing and hit-testing agree" (src/renderer.rs:542-546); grid size comes from
  `tile_content` (src/workspace.rs:1542-1545) inside `terminal_area` (1202-1213).
  `layout_tiles`, `rect_at_path`, `resize_hover_at` must survive every step.
- **Overlay modality invariant**: while an overlay is open, *nothing else* may paint
  interactive chrome — the existing pattern is `hot.clear()` (src/renderer.rs:1057-1059)
  and cursor nulling (585-587); the flyover layer currently violates it (§c bug 3).
  Element-tree ports must place modal trees above flyover with the same 0.30 scrim.
- **Canvas flyover ceiling**: the sidebar tree's height clip must keep tracking
  `App::flyover_rect_now` per frame (src/sidebar_ui.rs:197-204) or the flyover will
  underlap/overlap wrongly during its slide animation.
- **A11y font scaling**: `workspace::row_font_scale()` (src/workspace.rs:400-406, cap
  `MAX_ROW_FONT_SCALE` 388) for all chrome text; renderer caps at
  `MIN_FONT_SIZE`/`MAX_FONT_SIZE` (src/renderer.rs:296-297). Both must keep applying
  after the port.
- **Popout parity**: whatever the in-window flyover layer does, `FlyoverPopout`
  (src/main.rs:6260-6281) must keep painting the same layer from the shared App entity
  ("The sessions never move — only which surface paints them", 6255-6259) — including
  mouse-report buttons, scroll accumulation, and `popout_grabs_mouse` shift override.
- **Element-tree entry discipline**: src/ui/mod.rs re-export order (pub use above pub
  mod), rcn.toml import rewrite (crate::theme → crate::ui::theme), never `rcn init`.
