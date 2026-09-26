/**
 * opencode 2.x: the server half of roost's plugin — intentionally empty.
 *
 * v2 runs one shared background server for every opencode TUI, so a server
 * plugin never sees a pane's ROOST_PANE/ROOST_SOCK env and cannot tell panes
 * apart. The real work lives in tui.ts, which runs inside the pane's own
 * opencode process. This file exists only because v2 discovers a plugin
 * directory through its server entrypoint and drops one that has none.
 *
 * Installed by roost into ~/.config/opencode/plugin/roost/ when
 * `opencode --version` reports 2.x; see extensions/opencode-plugin.ts for 1.x.
 */
export default { id: "roost", setup() {} };
