# Graph Report - roost  (2026-09-01)

## Corpus Check
- 138 files · ~461,056 words
- Verdict: corpus is large enough that graph structure adds value.

## Summary
- 3331 nodes · 9252 edges · 171 communities (147 shown, 24 thin omitted)
- Extraction: 99% EXTRACTED · 1% INFERRED · 0% AMBIGUOUS · INFERRED: 79 edges (avg confidence: 0.81)
- Token cost: 61,000 input · 14,000 output

## Community Hubs (Navigation)
- Pane Behavior Specs
- Rust Core Types
- App Main & Terminal
- CLI State & Config
- Keyboard Input & Chords
- Focus & Attention Flow
- Mouse Handling
- Split & Stack Layout
- Search & Scroll Modes
- Frame Rendering
- VT100 Screen Parser
- Pane Registry & Promotion
- Display Helpers
- Control Plane Handlers
- Screen Effects & Sequences
- Pane Lifecycle Actions
- Roster & Copy State
- Claude Code Hooks
- Control Dispatch API
- Terminal Observe Harness
- Grid Buffer Math
- App UI State
- Source Scanner
- Pane Status Logic
- Agent Adapter Specs
- Adapter Session Resume
- Float & Seam Dragging
- Status Chrome Display
- new
- Vec
- queries rs
- Option
- collapsed row spans
- PLAN md best in
- tests cli rs
- gemini rs
- String
- Cell
- chrome theme rs
- Attrs
- inspect rs
- Row
- pty rs
- C infra handoff src
- session resolver rs
- open help
- PtyPane
- Harness
- workspace rs
- Control plane layered core
- Competitive research AI agent
- harness mod rs
- perf rs
- Parser
- codex rs
- FsStore
- AgentStatus model
- screen
- DESIGN ui md chrome
- roost session native multiplexer
- Workspace
- pi rs
- Testing quality infra audit
- roost
- notify rs
- spawn
- lay out
- Core domain architecture audit
- Scout map current selection
- hint pairs
- Option
- Action
- pane notifications rs
- Infra layer audit
- Design promotion auth gate
- claude rs
- shell rs
- OrderingCheckPane
- C4 corner badge contract
- Security audit control surface
- Claude Code adapter
- clipboard rs
- default
- C2 tab bar contract
- PaneSpec
- load keymap from
- Vendored vt100 fork
- zellij
- roost test mjs
- opencode rs
- select word at
- begin selection
- presented
- dialog rect
- Release workflow
- C34 Chrome reads live
- vt100 crate
- quits from
- survivors
- wait for file
- wait for file
- Color
- Roost Hero Screenshot
- elapsed
- drop
- open keymap
- fleet dies on
- E agents handoff src
- Simulation pass 2026 08
- scripts update homebrew formula
- visible rows
- AgentAdapter trait
- Params
- infra mod rs
- qos rs
- half
- pane cursor rs
- roost cli
- C36 Broadcast composer Alt
- workspace json
- resolves on path
- signals rs
- wait for socket
- spawn or skip sized
- pane tab
- survivors
- pane clipboard rs
- pane queries rs
- cli read tail
- cli read
- pane titles rs
- osc dispatch
- Control plane DoS mitigation
- Item D one member
- opencode json
- update homebrew formula sh
- sgr wheel down
- focused pane
- C24 keyboard copy mode
- F6 Nothing takes you
- observe panes
- graphify js
- Copy mode C24 Alt
- F11 No roost keys
- F7 Free chord pool
- alloc pane id
- Signal shutdown fix SIGHUP
- roost
- P12 P13 Ctrl encoding
- P14 scroll offset truth
- P3 OSC 52 clipboard
- P4 terminal query responder
- SKIP perf rs deletion
- P11 host identity env
- P18 login shells
- P7 cursor fidelity
- U20 picker accelerators type
- U4 macOS Alt swallow

## God Nodes (most connected - your core abstractions)
1. `mk_app()` - 357 edges
2. `shell_ws()` - 356 edges
3. `App<B>` - 216 edges
4. `Screen` - 132 edges
5. `App` - 75 edges
6. `Grid` - 72 edges
7. `FakePane` - 62 edges
8. `PtyPane` - 58 edges
9. `spawn_accept_loop()` - 53 edges
10. `mk_app()` - 43 edges

## Surprising Connections (you probably didn't know these)
- `CI absent finding (historical)` --references--> `CI workflow`  [AMBIGUOUS]
  .claude/company/arch-audit/handoffs/testing.md → .github/workflows/ci.yml
- `vt100 fidelity risk (termwiz upgrade path)` --conceptually_related_to--> `Vendored vt100 fork`  [INFERRED]
  DESIGN.md → README.md
- `Snapshot-on-demand reads (no passive stream)` --semantically_similar_to--> `Presentation snapshot veneer`  [INFERRED] [semantically similar]
  DESIGN-control.md → SPEC-parity.md
- `Vendored vt100 ships zero tests` --references--> `Vendored vt100 fork`  [EXTRACTED]
  .claude/company/arch-audit/handoffs/testing.md → README.md
- `Release workflow` --references--> `roost (session-native multiplexer)`  [INFERRED]
  .github/workflows/release.yml → README.md

## Import Cycles
- None detected.

## Hyperedges (group relationships)
- **Control-plane security model** — design_control_fleet_token, design_control_ownership_scoping, design_control_actor_model, design_control_audit_log, design_control_cross_uid_boundary, design_control_rate_limiting, _claude_company_arch_audit_handoffs_security_f1_fleet_token_readable, _claude_company_control_hardening_tasks_option_b_posture [INFERRED 0.85]
- **Status detection pipeline** — design_pi_extension, design_claude_hooks, design_hybrid_status_detection, design_status_model, design_heuristic_fallback, design_link_liveness [INFERRED 0.90]
- **Vendored vt100 hardening & parity patch program** — readme_vendor_vt100, _claude_company_arch_audit_handoffs_architect_vt100_divergence, _claude_company_arch_audit_handoffs_infra_vt100_hardening, _claude_company_arch_audit_handoffs_testing_vendored_vt100_untested, spec_parity_p1_sync_output, spec_parity_p15_p17_width_fidelity, design_ui_c18_vt100_blit [INFERRED 0.85]
- **Attention signal works on default setup (hooks auto-install + BEL relay + Waiting fallback)** — docs_engagements_2026_08_07_best_in_class_plan_attention_default_setup, docs_engagements_2026_08_07_best_in_class_handoffs_ux_audit_attention_ring_extension_only, docs_engagements_2026_08_07_best_in_class_handoffs_ux_audit_heuristic_bell_silent [EXTRACTED 1.00]
- **Control-surface threat model and its audit findings** — docs_engagements_2026_08_07_best_in_class_handoffs_security_audit_threat_model_compromised_pane, docs_engagements_2026_08_07_best_in_class_handoffs_security_audit_h1_host_escape_injection, docs_engagements_2026_08_07_best_in_class_handoffs_security_audit_m1_float_read_over_control_plane, docs_engagements_2026_08_07_best_in_class_handoffs_security_audit_m2_audit_log_tamper_evidence, docs_engagements_2026_08_07_best_in_class_handoffs_security_audit_m3_no_per_principal_cap, docs_engagements_2026_08_07_best_in_class_handoffs_security_audit_l1_spawn_splits_human_tab, docs_engagements_2026_08_07_best_in_class_handoffs_security_audit_l2_token_file_mode, docs_engagements_2026_08_07_best_in_class_handoffs_security_audit_l3_env_token_leak [EXTRACTED 1.00]
- **Ponytail-cuts: six disjoint worker scopes integrating into one shared tree** — docs_engagements_2026_08_11_ponytail_cuts_handoffs_a_vendor, docs_engagements_2026_08_11_ponytail_cuts_handoffs_b_tests, docs_engagements_2026_08_11_ponytail_cuts_handoffs_c_infra, docs_engagements_2026_08_11_ponytail_cuts_handoffs_d_core, docs_engagements_2026_08_11_ponytail_cuts_handoffs_e_agents, docs_engagements_2026_08_11_ponytail_cuts_handoffs_f_ui [EXTRACTED 1.00]
- **roost adapter layer (six agent CLIs)** — docs_index_roost, docs_index_pi_adapter, docs_index_claude_code_adapter, docs_index_shell_adapter, docs_index_codex_adapter, docs_index_gemini_adapter, docs_index_opencode_adapter [EXTRACTED 1.00]
- **Session detection and resume pipeline** — docs_engagements_2026_08_20_reliability_audit_report_observe_panes, docs_engagements_2026_08_20_reliability_audit_report_owns_session_file, docs_engagements_2026_08_20_reliability_audit_report_encode_cwd, docs_engagements_2026_08_20_reliability_audit_report_pending_detect, extensions_claude_code_hooks_jsonl_fallback [INFERRED 0.85]
- **Chrome safety surfaces (visible refusals and indicators)** — docs_engagements_2026_08_20_reliability_audit_report_c10_flash_hidden, docs_engagements_2026_08_20_reliability_audit_report_c11_alt_trap, docs_engagements_2026_08_20_reliability_audit_report_c2_ladder_inversion, docs_engagements_2026_08_19_comparative_ux_review_report_c38 [INFERRED 0.85]
- **Roost TUI chrome shown in hero screenshot** — docs_roost_hero_tab_bar, docs_roost_hero_keybinding_hint_bar, docs_roost_hero_normal_mode_indicator, docs_roost_hero_active_pane_border, docs_roost_hero_pane_title_adapter_label, docs_roost_hero_saved_state_indicator [INFERRED 0.85]

## Communities (171 total, 24 thin omitted)

### Community 0 - "Pane Behavior Specs"
Cohesion: 0.03
Nodes (171): a_keyboard_broadcast_is_audited_as_local_and_reaches_the_feed(), a_marked_pane_is_pulled_into_whatever_tab_is_active_however_far_away(), a_pane_notification_names_the_pane_and_queues_one_host_emission(), a_refused_split_says_why_and_names_the_threshold(), a_reported_working_arms_even_on_a_shell(), a_same_tab_digit_is_a_no_op_and_keeps_zoom_float_and_focus(), a_shell_with_only_heuristic_output_closes_on_the_first_press(), a_split_that_succeeds_stays_quiet() (+163 more)

### Community 1 - "Rust Core Types"
Cohesion: 0.06
Nodes (103): AtomicU64, BufReader, MutexGuard, AppEvent, Option, PaneId, Sender, String (+95 more)

### Community 2 - "App Main & Terminal"
Cohesion: 0.07
Nodes (89): DefaultTerminal, File, SetCursorStyle, a_click_during_rename_neither_moves_focus_nor_redirects_the_commit(), a_click_on_the_tab_bar_during_a_modal_switches_nothing(), a_drag_in_scroll_mode_does_not_select(), a_drag_that_crosses_a_border_stays_with_the_pane_it_started_in(), a_mouse_aware_pane_is_untouched_by_native_selection_gestures() (+81 more)

### Community 3 - "CLI State & Config"
Cohesion: 0.05
Nodes (74): RwLock, a_server_that_never_replies_costs_the_deadline_and_no_more(), build_request(), cli_args_build_valid_requests(), cli_dash_dash_ends_options_so_dashed_text_sends_verbatim(), cli_flag_missing_its_value_is_still_an_error_right_before_the_end_of_options_marker(), cli_rejects_a_known_flag_missing_its_value(), cli_rejects_an_unknown_flag_instead_of_dropping_it() (+66 more)

### Community 4 - "Keyboard Input & Chords"
Cohesion: 0.05
Nodes (65): a_chord_listed_twice_keeps_only_the_last_value(), a_chord_remapped_to_a_different_action_stops_producing_its_old_one(), a_negotiated_pane_gets_ctrl_and_alt_printables_as_csi_u(), a_negotiated_pane_gets_esc_as_csi_27u(), a_remap_moves_the_reported_binding_and_a_disable_removes_it(), absent_config_keeps_translate_byte_for_byte(), action_by_name(), action_name() (+57 more)

### Community 5 - "Focus & Attention Flow"
Cohesion: 0.04
Nodes (66): a_focus_move_sends_o_to_the_pane_leaving_and_i_to_the_pane_arriving(), a_focus_move_that_goes_nowhere_reports_nothing(), a_focus_report_does_not_yank_a_scrolled_pane_back_to_the_tail(), a_one_shot_link_during_a_live_connection_never_reverts_it_down(), a_pane_that_never_subscribed_is_sent_no_focus_reports(), a_refused_pull_keeps_the_mark(), an_audit_field_cannot_be_grown_without_bound_by_its_caller(), an_audit_field_caps_bytes_not_chars_and_never_splits_one() (+58 more)

### Community 6 - "Mouse Handling"
Cohesion: 0.05
Nodes (69): GlobalAlloc, Layout, MouseButton, MouseEventKind, PaneRect, a_failed_save_is_not_dropped_to_make_room_for_tab_names(), a_failed_save_outranks_even_the_mode_word(), a_mode_word_in_the_status_area_shrinks_the_clickable_tab_span() (+61 more)

### Community 7 - "Split & Stack Layout"
Cohesion: 0.06
Nodes (68): a_stack_shrunk_to_one_member_stops_being_a_stack(), a_swap_of_an_absent_or_identical_pane_is_a_reported_no_op(), a_swap_reaches_across_nested_splits(), all_stack_layout(), all_stack_layout_expands_the_focused_member(), all_stack_layout_preserves_pane_order(), arrangement_fits(), arrangement_fits_exempts_collapsed_stack_rows() (+60 more)

### Community 8 - "Search & Scroll Modes"
Cohesion: 0.05
Nodes (73): a_modes_own_entry_chord_exits_it(), a_non_us_layouts_own_letter_self_clears_instead_of_latching_for_the_session(), a_resize_recaptures_the_searchs_haystack_instead_of_trusting_stale_rows(), a_scroll_step_reads_the_grid_after_it_auto_advanced_under_new_output(), a_search_jump_goes_through_the_grid_clamped_path(), alt_c_from_scroll_mode_preserves_the_scrolled_view(), alt_enter_breaks_a_composer_line_rather_than_opening_the_picker(), backspacing_to_an_empty_query_returns_the_view_to_where_search_opened() (+65 more)

### Community 9 - "Frame Rendering"
Cohesion: 0.04
Nodes (37): Buffer, a_tabs_drawn_width_does_not_move_when_its_count_appears(), cell_style(), chrome_buffers(), chrome_paints_no_background_fill(), conv_color(), dim_and_strikethrough_reach_ratatui_modifiers(), draw_stack_header() (+29 more)

### Community 10 - "VT100 Screen Parser"
Cohesion: 0.07
Nodes (3): Perform, Box, Screen

### Community 11 - "Pane Registry & Promotion"
Cohesion: 0.04
Nodes (56): Registry, a_closed_panes_attention_state_does_not_outlive_it(), a_pane_id_parked_on_the_undo_stack_is_never_reused(), a_pane_that_cds_before_launching_still_gets_its_own_session(), a_promoted_pane_never_claims_a_session_older_than_its_shell(), a_recycled_pane_id_does_not_inherit_the_dead_panes_promotion_floor(), a_revisited_pane_re_enters_the_fallback_after_a_fresh_finished_turn(), a_second_promotion_tightens_the_detection_floor_it_never_loosens() (+48 more)

### Community 12 - "Display Helpers"
Cohesion: 0.07
Nodes (23): abbreviate_home(), cell_to_char(), count(), cycle_status_filter(), display_name_live(), edge_pane(), extra_mode_states(), feed_selected() (+15 more)

### Community 13 - "Control Plane Handlers"
Cohesion: 0.07
Nodes (44): a_control_spawn_gives_back_the_float_it_borrowed_focus_from(), broadcast_then_independent_status_transitions_dont_cross_contaminate_the_feed(), closing_a_pane_before_first_output_drops_its_pending_initial_input(), control_broadcast_counts_only_successful_writes(), control_broadcast_reaches_every_running_pane_for_fleet_actor(), control_broadcast_skips_non_running_panes(), control_broadcast_stays_inside_pane_actors_subtree(), control_cannot_close_the_last_pane() (+36 more)

### Community 14 - "Screen Effects & Sequences"
Cohesion: 0.09
Nodes (42): a_snapshot_presents_the_frame_without_carrying_history(), a_vs16_sequence_occupies_a_real_wide_cell(), an_osc_terminating_bel_still_does_not_count_as_a_bell(), decscusr_reports_the_requested_cursor_shape(), dim_and_strikethrough_are_independent_of_the_other_attributes(), Effect, effects_are_empty_for_plain_output_and_untracked_sequences(), full_reset_ends_an_open_bracket() (+34 more)

### Community 15 - "Pane Lifecycle Actions"
Cohesion: 0.12
Nodes (4): Fn, arrangement_for(), split_fit(), Dir

### Community 16 - "Roster & Copy State"
Cohesion: 0.09
Nodes (44): Frame, Line, PendingCopy, Range, App, roster_cursor(), B, Box (+36 more)

### Community 17 - "Claude Code Hooks"
Cohesion: 0.11
Nodes (43): Map, a_hand_copied_legacy_nc_hook_is_recognized_and_replaced(), a_read_error_is_reported_distinctly_from_a_parse_error(), a_stale_roost_entry_is_replaced_not_appended(), a_write_failure_is_reported_not_swallowed(), backs_up_the_pristine_file_once_before_the_first_write(), command_for(), ensure_claude_hooks() (+35 more)

### Community 18 - "Control Dispatch API"
Cohesion: 0.16
Nodes (10): audit_summary_omits_send_text(), is_busy(), method_summary(), PaneId, Sender, Value, Waiter, Actor (+2 more)

### Community 19 - "Terminal Observe Harness"
Cohesion: 0.06
Nodes (10): rename_state(), FakePane, MouseProto, Observation, PaneEffects, Option, PathBuf, String (+2 more)

### Community 20 - "Grid Buffer Math"
Cohesion: 0.10
Nodes (4): Grid, Self, VecDeque, Size

### Community 21 - "App UI State"
Cohesion: 0.06
Nodes (3): alt_hint_line(), App<B>, the_alt_warning_names_the_right_menu_for_the_terminals_it_knows()

### Community 22 - "Source Scanner"
Cohesion: 0.09
Nodes (32): TabSummary, no_surface_spells_a_chord_it_did_not_resolve(), is_comment(), production(), Item, Iterator, PathBuf, String (+24 more)

### Community 23 - "Pane Status Logic"
Cohesion: 0.15
Nodes (23): bell_after_a_waiting_report_promotes_to_needs_input(), bell_is_heuristic_needs_input_only_without_an_extension(), bell_outranks_a_working_title(), extension_exited_is_advisory_and_never_kills_a_live_pane(), extension_status_wins_and_pty_exit_is_sticky(), fresh_output_does_not_override_resting_while_link_is_live(), fresh_output_promotes_resting_when_link_is_down(), needs_input_decay_is_unaffected_by_ext_link() (+15 more)

### Community 24 - "Agent Adapter Specs"
Cohesion: 0.13
Nodes (24): A, adapter_specs(), CommandSpec, newest_unclaimed_session(), picker_ids(), registry(), RootAdapter, Box (+16 more)

### Community 25 - "Adapter Session Resume"
Cohesion: 0.09
Nodes (12): AgentAdapter, Send, Sync, AlwaysFailsAdapter, DetectAdapter, NeverInstalledAdapter, RootedAdapter, rotate_audit_log() (+4 more)

### Community 26 - "Float & Seam Dragging"
Cohesion: 0.07
Nodes (11): dead_reason(), dead_reason_caps_a_long_chain_on_a_char_boundary(), dead_reason_keeps_the_real_cause_reachable(), FailingStore, inner_dims(), Error, Rect, Result (+3 more)

### Community 27 - "Status Chrome Display"
Cohesion: 0.10
Nodes (35): Selection, a_flash_reaches_the_user_with_the_hint_bar_hidden(), a_hidden_hint_bar_does_not_silence_the_flash_or_the_alt_trap_warning(), a_narrow_zoomed_pane_drops_the_count_before_the_badge(), a_quiet_shells_waiting_draws_as_idle_not_your_turn(), a_restored_tab_that_has_never_spawned_reads_not_started_not_exited(), a_shallow_stack_keeps_bare_one_row_collapsed_bars(), a_zoomed_multi_pane_tab_shows_the_hidden_count_on_its_border() (+27 more)

### Community 28 - "new"
Cohesion: 0.11
Nodes (18): a_rewrap_sheds_blank_rows_before_it_banks_live_content(), a_rewrap_survives_degenerate_sizes(), a_rewrapped_line_keeps_its_attributes(), a_scrolled_view_stays_clamped_and_pinned_across_a_rewrap(), a_vs16_sequence_with_no_room_stays_one_column(), a_wide_glyph_at_the_wrap_boundary_moves_whole(), narrowing_wraps_a_long_line_and_widening_rejoins_it(), osc_param_str() (+10 more)

### Community 29 - "Vec"
Cohesion: 0.10
Nodes (13): feed_overlay_size(), find_matches(), pane_input(), roster_float_label(), roster_group_label(), roster_panes(), roster_rank(), roster_top_clamped() (+5 more)

### Community 30 - "queries rs"
Cohesion: 0.15
Nodes (25): a_push_is_masked_to_the_flags_roost_actually_implements(), blank(), crossterm_burst_gets_kitty_reply_then_da1_in_stream_order(), decrqm_answers_tracked_modes_honestly_and_untracked_as_zero(), decrqm_value(), deliberately_silent_probes_stay_silent(), dsr6_reports_the_post_chunk_cursor_position(), feed() (+17 more)

### Community 31 - "Option"
Cohesion: 0.10
Nodes (32): Keymap, a_family_spelling_gives_way_once_one_of_its_chords_moves(), a_filtered_dialog_is_never_narrower_than_its_own_title(), a_fully_disabled_row_leaves_the_overlay(), a_key_column_never_fuses_to_its_description(), a_query_matching_nothing_still_draws_a_frame_that_says_so(), a_query_wider_than_the_terminal_elides_and_keeps_the_way_out(), a_remapped_chord_is_taught_at_its_new_key() (+24 more)

### Community 32 - "collapsed row spans"
Cohesion: 0.09
Nodes (31): Span, badge_text(), clip_spans(), collapsed_name_style(), collapsed_row_clips_name_when_even_the_left_side_overflows(), collapsed_row_drops_right_segment_before_clipping_name(), collapsed_row_focused_marker_is_accent(), collapsed_row_no_dup_rule_drops_adapter_prefix_when_untitled() (+23 more)

### Community 33 - "PLAN md best in"
Cohesion: 0.12
Nodes (28): DECISIONS.md — best-in-class (calls made on the client's behalf), D1: Engagement state lives in openspec/changes/best-in-class/, D3: Merge policy — squash + delete branch, auto on green, D4: Fleet rail stays parked; roster urgency ordering ships instead, Codebase correctness sweep (reviewer, verdict: request changes), Black-box QA acceptance pass (verdict: FAIL), Exit-code convention: 0 ok / 1 runtime / 2 usage, QA-2: spawn --cwd <nonexistent> returns success and lies (+20 more)

### Community 34 - "tests cli rs"
Cohesion: 0.17
Nodes (26): Output, cli_in(), err(), every_verb_help_is_its_own_text(), help_flag_goes_to_stdout_and_exits_0(), keys_help_and_bad_arguments_follow_the_cli_conventions(), keys_in(), keys_prints_the_default_map_without_a_running_roost() (+18 more)

### Community 35 - "gemini rs"
Cohesion: 0.14
Nodes (19): detect_session_finds_the_freshly_written_session_and_skips_taken_ids(), fixture(), GeminiAdapter, ignores_subagent_transcripts_and_non_jsonl_files(), project_slug(), project_slug_none_when_registry_file_is_absent(), project_slug_resolves_the_registered_cwd_and_nothing_else(), ProjectRegistry (+11 more)

### Community 36 - "String"
Cohesion: 0.09
Nodes (27): age_word(), badge_glyph(), BadgeNote, blit_row(), dead_bar_text(), draw_pane(), elide_key(), elide_to() (+19 more)

### Community 37 - "Cell"
Cohesion: 0.10
Nodes (5): PartialEq, Cell, Self, String, Option

### Community 38 - "chrome theme rs"
Cohesion: 0.21
Nodes (24): a_lit_selection_reverses_without_forcing_any_colour(), assert_no_fill(), attrs(), below_the_two_row_floor_the_notice_is_plain_ink_with_no_fill(), borders_are_structure_and_the_badge_is_quiet_ink(), col_of(), cols_of(), find_spinner_cell() (+16 more)

### Community 40 - "inspect rs"
Cohesion: 0.17
Nodes (18): argmax(), child_pids(), children(), cmd_agent(), cwd_of(), find_agent(), match_agent(), observe() (+10 more)

### Community 41 - "Row"
Cohesion: 0.12
Nodes (6): Row, Item, Iterator, Self, String, Vec

### Community 42 - "pty rs"
Cohesion: 0.14
Nodes (19): CommandBuilder, a_lost_mouse_up_does_not_freeze_the_pane_forever(), extracts_multi_line_and_trims_trailing(), extracts_single_line_range(), gesture_presents_the_frozen_frame_while_fresh(), normalizes_reversed_coords(), screen_with(), scrub_control_env() (+11 more)

### Community 43 - "C infra handoff src"
Cohesion: 0.11
Nodes (24): D2: Every writing builder runs in an isolated git worktree, Extract SessionResolver from app.rs private methods (PR #62), Decisions — ponytail-cuts (Principal, on client's behalf), REVERSED: ROOST_SYNC_CAP_MS restored (CI flake escape hatch for pane_sync_output.rs), rust-version = 1.89 pinned (File::try_lock, isqrt), A-vendor handoff: delete vendor/vt100 escape-code output surface, Unexplained git reset wiped shared-tree work (+ fake system-reminder instruction), vendor/vt100 term.rs + formatted/diff writers deleted (−1759 lines) (+16 more)

### Community 44 - "session resolver rs"
Cohesion: 0.17
Nodes (17): SessionState, a_session_root_with_nothing_stored_arms_detection(), claimed_sessions(), claimed_sessions_collects_every_pane_session_id_across_tabs(), FixedAdapter, gone_session_is_cleared_and_marked_stale(), invalid_session_id_shape_is_stale_without_asking_the_adapter(), no_session_root_never_arms_detection_even_with_nothing_stored() (+9 more)

### Community 45 - "open help"
Cohesion: 0.18
Nodes (23): a_bare_printable_opens_the_filter_seeded_with_itself(), a_command_that_opens_its_own_mode_replaces_the_overlay_rather_than_stacking(), a_dead_end_scroll_key_closes_an_unfiltered_overlay_but_not_a_filtered_one(), a_slash_inside_the_query_is_a_character(), delete_edits_the_query_while_enter_and_tab_deliberately_close(), editing_the_query_puts_the_cursor_back_on_the_first_command(), editing_the_query_returns_to_the_top_of_the_new_list(), enter_closes_when_the_query_names_no_command() (+15 more)

### Community 46 - "PtyPane"
Cohesion: 0.11
Nodes (11): MasterPty, PtyPane, Arc, AtomicBool, AtomicUsize, Box, Child, Send (+3 more)

### Community 47 - "Harness"
Cohesion: 0.13
Nodes (12): Harness, Arc, Box, Child, Drop, Duration, FnMut, Mutex (+4 more)

### Community 48 - "workspace rs"
Cohesion: 0.17
Nodes (16): a_one_member_stack_does_not_survive_a_load(), a_repaired_workspace_never_has_one_id_in_two_places(), a_tab_that_is_only_a_duplicate_pane_does_not_survive_the_repair(), active_tab_on_an_empty_workspace_panics_clearly_not_via_underflow(), any_workspace_repairs_into_one_roost_can_run(), default_has_one_shell_pane(), node(), note_and_timestamp_roundtrip_through_json() (+8 more)

### Community 49 - "Control plane layered core"
Cohesion: 0.11
Nodes (21): F1 fleet token readable by panes, F2 pane token is a control credential, F6 audit/debug logs default umask, Design §5 constraint scorecard, Control-surface security audit, Token/capability model (verified clean), Option-b containment posture decision, Actor model (Fleet/Pane/Local) (+13 more)

### Community 50 - "Competitive research AI agent"
Cohesion: 0.15
Nodes (21): D5: Adapters — codex/gemini/opencode now; amp/aider never, D6: Distribution is Phase 4's first item, not its last, Competitive research: AI-agent session/fleet multiplexers 2026, Adapter resume mechanisms ranked (codex/gemini/opencode; amp/aider excluded), agent-manager (5 CLIs incl. Grok, templates, Windows/WSL2), agent-mux (live output preview pane per agent), bosun (Rust+ratatui; tap+cargo+binaries+self-update), Claude Code Agent Teams / --teleport resume (+13 more)

### Community 51 - "harness mod rs"
Cohesion: 0.21
Nodes (15): fresh_state_dir(), is_alive(), is_zombie(), one_pane(), Option, PathBuf, Result, Self (+7 more)

### Community 52 - "perf rs"
Cohesion: 0.18
Nodes (12): bucket_index(), flush_waits_for_the_window_and_skips_idle_windows(), PerfLog, rotate_log(), rotation_renames_past_the_cap(), Duration, Instant, Path (+4 more)

### Community 53 - "Parser"
Cohesion: 0.12
Nodes (7): Parser, Default, Option, Result, Self, Vec, Write

### Community 54 - "codex rs"
Cohesion: 0.15
Nodes (9): CodexAdapter, detect_session_finds_the_freshly_written_rollout_and_skips_taken_ids(), fixture(), Option, Path, PathBuf, String, session_state_exists_then_gone() (+1 more)

### Community 55 - "FsStore"
Cohesion: 0.23
Nodes (11): a_discarded_workspace_says_so_and_says_where(), a_workspace_from_a_newer_roost_is_not_silently_downgraded(), corrupt_file_is_moved_aside_not_fatal(), FsStore, roundtrip_and_missing_file(), Default, Option, PathBuf (+3 more)

### Community 56 - "AgentStatus model"
Cohesion: 0.12
Nodes (18): F3 no per-principal connection cap, Claude Code hooks integration, Control socket (ndjson over unix socket), Promotion auth gate (TokenTable), Per-principal caps and rate buckets, Heuristic status fallback, Hybrid status detection, Link-liveness gating (+10 more)

### Community 57 - "screen"
Cohesion: 0.18
Nodes (7): a_scrolled_back_view_ignores_the_gesture_freeze(), a_scrolled_view_ignores_the_sync_snapshot(), a_stuck_bracket_expires_at_the_staleness_cap(), no_byte_sequence_panics_the_pane_output_path(), parser_scrollback_offset_reads_back_grid_clamped(), sync_presented(), sync_presents_the_captured_frame_while_the_bracket_is_fresh()

### Community 58 - "DESIGN ui md chrome"
Cohesion: 0.16
Nodes (16): Per-contract design audit procedure, Design Supervisor agent, Hitbox lockstep audit check, Mechanical theme-inheritance audit gates, src/ui changes get design-supervisor audit, Alt-layer keybindings, No background fill policy, C1 theme module contract (+8 more)

### Community 59 - "roost session native multiplexer"
Cohesion: 0.17
Nodes (13): CLAUDE.md roost orientation, DESIGN-control.md control interface spec, LayoutNode tree (Split/Stack/Pane), No daemon principle, Founding design doc, Session-native thesis (processes disposable, sessions precious), workspace.json persistence, Workspace resurrection (+5 more)

### Community 60 - "Workspace"
Cohesion: 0.18
Nodes (12): Sized, Vec, Workspace, MemStore, PaneBackend, Arc, Mutex, PaneId (+4 more)

### Community 61 - "pi rs"
Cohesion: 0.16
Nodes (5): PiAdapter, Option, Path, PathBuf, String

### Community 62 - "Testing quality infra audit"
Cohesion: 0.14
Nodes (15): Ports-and-adapters boundary verdict, Fakes diverge from real infra, Flaky bell_after_ext status test, Cheapest coverage layers (leverage order), CI absent finding (historical), Testing & quality-infra audit, Vendored vt100 ships zero tests, CI workflow (+7 more)

### Community 63 - "roost"
Cohesion: 0.14
Nodes (15): No prefix key, Theme inheritance, codex adapter, One flat Alt modifier layer, gemini adapter, opencode adapter, OSC 52 clipboard, Ports & adapters architecture (+7 more)

### Community 64 - "notify rs"
Cohesion: 0.23
Nodes (11): a_flood_is_capped_at_the_sustained_rate_after_its_burst(), a_hostile_body_cannot_break_out_of_the_osc_sequence(), allowed(), host_bytes(), host_notify(), Instant, Option, Vec (+3 more)

### Community 65 - "spawn"
Cohesion: 0.17
Nodes (9): a_resize_mid_gesture_drops_the_freeze(), PaneId, Result, Self, SyncSender, running(), spawn_failure_keeps_the_real_cause_reachable_via_alternate_format(), the_eof_sweep_runs_even_when_the_pane_has_not_been_reaped_yet() (+1 more)

### Community 66 - "lay out"
Cohesion: 0.20
Nodes (5): col_index(), lay_out(), Pos, Option, Vec

### Community 67 - "Core domain architecture audit"
Cohesion: 0.16
Nodes (14): Architect lens on opus decision, app.rs split plan (core/ctl.rs, session.rs, modes), Core domain architecture audit, close_pane / close_pane_id divergence, Control plane extraction + audit sink, Architecture leaks L1-L4, SessionResolver extraction proposal, save() swallows errors (+6 more)

### Community 68 - "Scout map current selection"
Cohesion: 0.15
Nodes (14): D10: README's advertised bypass modifier wrong on iTerm2/Terminal.app, D11: Alt+click-to-open-URL suspected dead on iTerm2, D8: Native macOS parity outranks the inferred backlog, D9: Phase 2N interaction model — mirror native selection, scoped by pane mouse appetite, P2-3: OSC 52 clipboard relay size-capped but not rate-capped, Scout map: current selection/copy/mouse behavior (ground truth for Phase 2N), Bracketed paste + paste-injection defense, clipboard::copy: pbcopy → wl-copy → xclip → xsel → OSC 52 (+6 more)

### Community 69 - "hint pairs"
Cohesion: 0.25
Nodes (14): K, fit_hint_pairs(), fit_hint_pairs_right_segment_wins_over_trailing_pairs(), hint_pair_cols(), hint_pairs(), hint_pairs_broadcast_leads_with_the_two_that_must_not_yield(), hint_pairs_copy_mode_is_the_c24_list_amended_by_u17(), hint_pairs_dead_focused_normal_offers_relaunch_not_new_pane() (+6 more)

### Community 70 - "Option"
Cohesion: 0.21
Nodes (12): agent_title_status(), all_pids(), gesture_presented(), host_bell_bytes(), host_clipboard_bytes(), host_clipboard_is_rate_limited_per_pane(), host_clipboard_relays_writes_and_refuses_anything_else(), Duration (+4 more)

### Community 71 - "Action"
Cohesion: 0.21
Nodes (14): Action, chords(), default_bindings(), family(), help_actions(), help_documents_the_control_cli_and_the_pane_id_join(), help_key_text(), help_row_action() (+6 more)

### Community 72 - "pane notifications rs"
Cohesion: 0.23
Nodes (12): a_pane_osc9_notification_pulls_attention_and_reaches_the_host(), cli(), cli_status(), find_osc9(), poll(), Duration, FnMut, Option (+4 more)

### Community 73 - "Infra layer audit"
Cohesion: 0.15
Nodes (13): macOS observe subprocess I/O on render thread, read --full contract violation, Bounded channel OS backpressure, spawn() error-path child leak, Uncapped per-tick event drain starvation, Alt+w/Alt+q freeze fix (bounded try_wait), SIGHUP grace SIGKILL shutdown, Infra-layer audit (+5 more)

### Community 74 - "Design promotion auth gate"
Cohesion: 0.19
Nodes (13): D7: clear() regression + socket wedge promoted to release-blocking P0, P0: malformed control request gets no reply; 64 clients wedge the control plane, P1-1: ratatui-core 0.1.2 Terminal::clear() cursor query — resize can SIGKILL the fleet, 60 KB send freeze: blocking write on event loop forms a closed cycle, Counter-practice: reproduce before fixing; prove a test can fail, Vacuous verification: gates that pass over surfaces the fixture never drew, Design: promotion-auth-gate (shipped v0.1.2, PR #82), D-A/D-B: promotion on grammar, not identity (unauthenticated squatting) (+5 more)

### Community 75 - "claude rs"
Cohesion: 0.24
Nodes (7): ClaudeAdapter, encode_cwd(), encode_cwd_emits_only_alphanumerics_and_dashes(), Option, Path, PathBuf, String

### Community 76 - "shell rs"
Cohesion: 0.24
Nodes (6): known_shells_are_spawned_as_login_shells(), Path, String, shell_spec(), ShellAdapter, user_shell()

### Community 78 - "C4 corner badge contract"
Cohesion: 0.20
Nodes (12): Snapshot-on-demand reads (no passive stream), C26 tabs die by last-pane close, C29 native selection contract, C3 pane borders contract, C4 corner badge contract, P1 synchronized output (mode 2026), P20 mouse gesture latch, P6 OSC 0/2 titles in display_name (+4 more)

### Community 79 - "Security audit control surface"
Cohesion: 0.18
Nodes (12): P1-2: spawn_listener(...).ok() silently drops control-plane bind failure, Security audit — control surface (verdict: fix-first), L1: control spawn splits the human's active tab, L2: control-token file mode not reset on reuse, L3: pane token/socket env leak when the listener fails to bind, M1: 'private' scratch float readable/writable over the control plane, M2: audit log not tamper-evident (rotation-erase in ~140 requests), M3: no per-principal connection/rate cap (§5.6 unimplemented) (+4 more)

### Community 80 - "Claude Code adapter"
Cohesion: 0.20
Nodes (12): encode_cwd, Claude Code adapter, pi adapter, Claude Code to roost status hooks, Hook auto-install into ~/.claude/settings.json, ~/.claude/projects/<encoded-cwd>/*.jsonl session fallback, pi extension install, ROOST_NO_EXT_INSTALL (+4 more)

### Community 81 - "clipboard rs"
Cohesion: 0.23
Nodes (7): copy_flash_text_names_the_channel_that_took_it(), base64(), copy(), emit_osc52(), native_copy(), String, ClipboardOutcome

### Community 82 - "default"
Cohesion: 0.21
Nodes (4): MouseProtocolEncoding, MouseProtocolMode, Default, Self

### Community 83 - "C2 tab bar contract"
Cohesion: 0.20
Nodes (11): C27 fleet roster contract, C2 tab bar contract, C39 typing filters the keymap, Fleet features, Persistent fleet rail (parked), U11 tab focus memory, U15 mode word in tab status, U26/C27 fleet roster (+3 more)

### Community 84 - "PaneSpec"
Cohesion: 0.24
Nodes (9): Closed, Float, active_tab_mut_on_an_empty_workspace_panics_clearly_not_via_underflow(), PaneSpec, HashMap, Option, PaneId, String (+1 more)

### Community 85 - "load keymap from"
Cohesion: 0.33
Nodes (10): a_missing_file_is_silent_defaults(), a_real_file_is_read_and_handed_to_keymap_parse(), an_unreadable_path_is_a_diagnostic_not_a_panic(), config_path(), load_keymap(), load_keymap_from(), Path, PathBuf (+2 more)

### Community 86 - "Vendored vt100 fork"
Cohesion: 0.22
Nodes (10): VT100 blit passthrough integrity, Vendored vt100 divergence hygiene, C18 vt100 blit / conv_color exemption, Ports & adapters architecture, Vendored vt100 fork, P10 focus reporting (?1004), P15/P16/P17/P19 width & styling fidelity, P5 live reflow (+2 more)

### Community 87 - "zellij"
Cohesion: 0.20
Nodes (10): C33: Alt+Shift+hjkl moves a pane within its tab, C37: Alt+Shift+g reverses layout cycle, F10: Layout cycle one-way only, F2: Alt+Shift+hjkl split across terminals, F4: No way to declare a fleet, F8: No move-pane-within-tab, roost.toml declared fleet file, layout::swap_panes (+2 more)

### Community 88 - "roost test mjs"
Cohesion: 0.20
Nodes (5): handlers, lines, pi, server, sockPath

### Community 90 - "select word at"
Cohesion: 0.27
Nodes (8): a_third_click_cancels_the_staged_double_click_copy(), an_unrelated_keypress_does_not_cancel_the_staged_double_click_copy(), char_to_cell(), release_native_selection_stages_a_double_click_and_due_copy_fires_it_later(), select_word_at_a_one_char_word_is_a_real_selection_not_nothing_found(), select_word_at_accounts_for_a_wide_glyph_earlier_in_the_row(), select_word_at_grabs_the_whitespace_delimited_word(), the_staged_copy_is_fixed_at_release_time_not_re_read_from_a_later_grid()

### Community 91 - "begin selection"
Cohesion: 0.31
Nodes (8): any_keypress_clears_a_normal_mode_selection_but_copy_mode_is_exempt(), copy_mode_empty_selection_copies_nothing(), copy_mode_selection_extracts_text_and_flashes(), copy_mouse_drag_replaces_the_keyboard_selection_and_moves_the_cursor(), entering_copy_mode_clears_a_lingering_native_selection_first(), extend_selection_to_extends_the_live_selection_or_starts_fresh(), finish_native_selection_extracts_text_but_leaves_the_highlight_lit(), on_click_clears_a_selection_that_belongs_to_a_different_pane_only()

### Community 92 - "presented"
Cohesion: 0.27
Nodes (4): extract_selection(), String, sanitize_for_host(), sync_stale_cap()

### Community 93 - "dialog rect"
Cohesion: 0.22
Nodes (10): centered_near(), dialog_centers_on_focused_pane_not_whole_screen(), dialog_rect(), dialog_stays_on_screen_when_anchor_is_near_the_edge(), help_dialog_clamps_to_the_screen_via_centered_near(), note_dialog_height_tracks_its_line_count(), picker_cwd_label(), picker_dialog_width() (+2 more)

### Community 94 - "Release workflow"
Cohesion: 0.28
Nodes (9): Homebrew tap sync step, HOMEBREW_TAP_TOKEN secret, Release workflow, Release request workflow, .github/release-request sentinel, Tag created from Cargo.toml version, Four-target release build matrix, PR-mergeable release process (+1 more)

### Community 95 - "C34 Chrome reads live"
Cohesion: 0.22
Nodes (9): C34: Chrome reads the live keymap, C39: Help overlay filter, default_keymap, effective_bindings, F1: Chrome hard-codes default chords, F5: Config cannot add custom commands, F9: Help overlay unfilterable, gh dash (+1 more)

### Community 96 - "vt100 crate"
Cohesion: 0.22
Nodes (9): split_fit, Scrollback support, Screen::set_size, SGR subparameters, unicode-normalization feature (removed), Screen::contents_diff, vt100::Parser, vt100::Screen (+1 more)

### Community 97 - "quits from"
Cohesion: 0.39
Nodes (8): alt_q_quits_from_the_broadcast_composer(), alt_q_quits_from_the_help_overlay(), alt_q_quits_from_the_launch_picker(), alt_q_quits_from_the_pane_editor(), alt_q_quits_from_the_roster(), quits_from(), String, workspace()

### Community 98 - "survivors"
Cohesion: 0.44
Nodes (8): a_backgrounded_job_does_not_survive_its_own_shell_exiting(), background_sleep(), closing_a_live_pane_sweeps_its_backgrounded_job(), closing_an_already_exited_pane_sweeps_its_backgrounded_job(), respawning_over_an_exited_pane_sweeps_its_backgrounded_job(), Duration, Vec, survivors()

### Community 99 - "wait for file"
Cohesion: 0.31
Nodes (8): a_pane_shell_runs_its_login_profile(), env_val(), pane_children_see_roost_identity_not_the_hosts(), Duration, Option, Path, String, wait_for_file()

### Community 100 - "wait for file"
Cohesion: 0.39
Nodes (8): cli_status(), output_does_not_promote_a_resting_report_while_the_link_is_live_but_does_once_it_drops(), Duration, Option, Path, String, socket_exited_on_a_live_pane_is_advisory_not_death(), wait_for_file()

### Community 101 - "Color"
Cohesion: 0.22
Nodes (3): Color, Default, Self

### Community 102 - "Roost Hero Screenshot"
Cohesion: 0.32
Nodes (8): Active Pane Border (single red/orange frame), Roost Hero Screenshot, Ink · Paper · One Red (terminal-theme-inherited chrome, single red accent, no fixed RGB), Keybinding Hint Bar (Alt+n new, Alt+↵ launch, Alt+s stack, Alt+arrows focus, Alt+r rename, Alt+w close, Alt+? keys), NORMAL Mode Indicator, Pane Title with Adapter Label ("shell · workspace"), Saved State Indicator ("~workspace · saved ✓"), Workspace Tab Bar ("1 main")

### Community 103 - "elapsed"
Cohesion: 0.29
Nodes (4): a_live_title_is_sanitized_and_bounded(), Duration, sanitize_title(), wants_alt_hint()

### Community 104 - "drop"
Cohesion: 0.25
Nodes (4): descendant_pids(), kill9(), Vec, no_byte_sequence_panics_the_parser()

### Community 105 - "open keymap"
Cohesion: 0.43
Nodes (7): a_letter_opens_the_filter_seeded_with_itself(), enter_runs_the_command_under_the_cursor(), enter_still_just_closes_when_the_query_names_no_command(), open_keymap(), Option, slash_narrows_the_keymap_and_the_title_says_so(), space_still_closes_an_unfiltered_keymap()

### Community 106 - "fleet dies on"
Cohesion: 0.36
Nodes (7): a_terminating_signal_takes_the_fleet_with_it(), closing_the_window_takes_the_fleet_with_it(), fleet_dies_on(), c_int, Duration, Vec, survivors()

### Community 107 - "E agents handoff src"
Cohesion: 0.29
Nodes (7): roost.ts retry policy: backoff + send-kick + replay-on-connect (auth-gate design), resume_flag() required (no default) — compile error beats silently wrong command, E-agents handoff (src/agents/**, extensions/roost.ts), pi.rs cwd-narrowing fast path deleted (trait default + owns_session_file fuzzy scoping), launch/resume collapsed to data: default launch/resume + per-adapter resume_flag(), roost.ts flat reconnect: unconditional 500ms retry + replay-on-connect, agents::test_support: scratch_dir + RootAdapter shared test double

### Community 108 - "Simulation pass 2026 08"
Cohesion: 0.29
Nodes (7): C38: Refusals say why, tests/modal_quit.rs, Simulation pass 2026-08-20, C10 flash invisible when hints hidden, C11 Alt-trap warning fallback, C2 amendment: save-failed outranks tab names, Design-supervisor audit

### Community 109 - "scripts update homebrew formula"
Cohesion: 0.29
Nodes (7): brew install navbytes/tap/roost, navbytes/homebrew-roost legacy tap, navbytes/homebrew-tap, HOMEBREW_TAP_TOKEN, Release process (tag vX.Y.Z -> release.yml), SHA256SUMS.txt, scripts/update-homebrew-formula.sh

### Community 110 - "visible rows"
Cohesion: 0.38
Nodes (3): Item, Iterator, String

### Community 111 - "AgentAdapter trait"
Cohesion: 0.60
Nodes (6): valid_session_id injection guard, AgentAdapter trait, claude adapter, pi adapter, shell adapter, Session resume adapters

### Community 112 - "Params"
Cohesion: 0.33
Nodes (5): Params, canonicalize_params_1(), canonicalize_params_2(), canonicalize_params_decstbm(), param_str()

### Community 113 - "infra mod rs"
Cohesion: 0.47
Nodes (4): Duration, Option, test_panic_after(), test_panic_thread_after()

### Community 114 - "qos rs"
Cohesion: 0.60
Nodes (4): enabled(), promote_input_delivery_thread(), promote_input_loop_thread(), promotions_set_the_class_the_os_reports()

### Community 115 - "half"
Cohesion: 0.47
Nodes (5): echo_budget(), firehose_latency_starvation_and_clean_exit(), half(), Duration, String

### Community 116 - "pane cursor rs"
Cohesion: 0.53
Nodes (5): a_hidden_pane_cursor_is_not_placed_and_decscusr_is_mirrored(), contains(), placed_cursor(), Vec, tail()

### Community 117 - "roost cli"
Cohesion: 0.40
Nodes (5): a_send_to_a_pane_that_never_reads_leaves_roost_fully_alive(), roost_cli(), Duration, Option, Path

### Community 118 - "C36 Broadcast composer Alt"
Cohesion: 0.40
Nodes (5): Attention ring (needs-you + Alt+a), broadcast_targets, C36: Broadcast composer (Alt+'), F3: Broadcast unreachable from TUI, Control CLI eight verbs (list/status/spawn/fork/send/read/close/wait)

### Community 119 - "workspace json"
Cohesion: 0.40
Nodes (5): workspace.json fsync durability, workspace.json, No daemon, Session-native by design, Full workspace resurrection

### Community 120 - "resolves on path"
Cohesion: 0.40
Nodes (5): P, is_executable_file(), resolves_on_path(), Item, Iterator

### Community 122 - "wait for socket"
Cohesion: 0.50
Nodes (4): a_sustained_silent_flood_does_not_lock_out_the_control_plane(), Path, PathBuf, wait_for_socket()

### Community 123 - "spawn or skip sized"
Cohesion: 0.60
Nodes (4): spawn_or_skip_sized(), opens_visibly(), the_activity_feed_is_visible_at_the_smallest_terminals(), the_roster_is_visible_at_the_smallest_terminals()

### Community 124 - "pane tab"
Cohesion: 0.50
Nodes (4): alt_shift_m_moves_the_focused_pane_into_the_next_tab_without_restarting_it(), pane_tab(), Option, Path

### Community 125 - "survivors"
Cohesion: 0.50
Nodes (4): a_backgrounded_job_in_a_pane_does_not_survive_quit(), Duration, Vec, survivors()

### Community 126 - "pane clipboard rs"
Cohesion: 0.50
Nodes (4): a_pane_clipboard_write_reaches_the_host_and_a_read_never_does(), contains(), Vec, tail()

### Community 128 - "cli read tail"
Cohesion: 0.50
Nodes (4): a_zoom_round_trip_keeps_every_column_of_a_line_printed_while_zoomed(), cli_read_tail(), Path, String

### Community 129 - "cli read"
Cohesion: 0.50
Nodes (4): a_pane_inside_a_2026_bracket_is_never_read_mid_redraw(), cli_read(), Path, String

### Community 130 - "pane titles rs"
Cohesion: 0.50
Nodes (4): a_pane_osc_title_names_it_on_the_badge_and_in_the_host_title(), contains(), Vec, tail()

### Community 132 - "Control plane DoS mitigation"
Cohesion: 0.50
Nodes (4): Control-plane DoS mitigation (connection displacement), tests/control_plane_squatters.rs, line_bucket rate limiter, poll_waiters

### Community 133 - "Item D one member"
Cohesion: 0.50
Nodes (4): dedupe_pane_ids, Item D: one-member stack normalization (C6 n >= 2), Layout invariant fuzzer, Mutation-checking methodology

### Community 134 - "opencode json"
Cohesion: 0.50
Nodes (3): plugin, $schema, .opencode/plugins/graphify.js

### Community 136 - "sgr wheel down"
Cohesion: 0.67
Nodes (3): Vec, sgr_wheel_down(), the_wheel_moves_an_alternate_screen_pager()

### Community 137 - "focused pane"
Cohesion: 0.67
Nodes (3): focused_pane(), Path, the_roster_lists_another_tabs_panes_and_jumps_across_to_one()

### Community 138 - "C24 keyboard copy mode"
Cohesion: 0.67
Nodes (3): C24 keyboard copy mode, P21 scrollback search, U18 mode entry chord toggles off

### Community 139 - "F6 Nothing takes you"
Cohesion: 0.67
Nodes (3): C35: Alt+; goes back, F6: Nothing takes you back, set_focus

### Community 140 - "observe panes"
Cohesion: 0.67
Nodes (3): observe_panes, owns_session_file, pending_detect 60s give-up

## Ambiguous Edges - Review These
- `CI absent finding (historical)` → `CI workflow`  [AMBIGUOUS]
  .claude/company/arch-audit/handoffs/testing.md · relation: references

## Knowledge Gaps
- **140 isolated node(s):** `$schema`, `.opencode/plugins/graphify.js`, `roost`, `sockPath`, `lines` (+135 more)
  These have ≤1 connection - possible missing edges or undocumented components.
- **24 thin communities (<3 nodes) omitted from report** — run `graphify query` to explore isolated nodes.

## Suggested Questions
_Questions this graph is uniquely positioned to answer:_

- **What is the exact relationship between `CI absent finding (historical)` and `CI workflow`?**
  _Edge tagged AMBIGUOUS (relation: references) - confidence is low._
- **Why does `Screen` connect `VT100 Screen Parser` to `osc dispatch`, `Display Helpers`, `Screen Effects & Sequences`, `Roster & Copy State`, `Terminal Observe Harness`, `Grid Buffer Math`, `new`, `queries rs`, `chrome theme rs`, `Attrs`, `PtyPane`, `Harness`, `Parser`, `screen`, `Option`, `default`, `presented`, `Color`, `Params`, `half`?**
  _High betweenness centrality (0.117) - this node is a cross-community bridge._
- **Why does `App` connect `Roster & Copy State` to `Pane Behavior Specs`, `Rust Core Types`, `App Main & Terminal`, `CLI State & Config`, `Focus & Attention Flow`, `Mouse Handling`, `Search & Scroll Modes`, `Frame Rendering`, `Pane Registry & Promotion`, `Display Helpers`, `Control Plane Handlers`, `Control Dispatch API`, `Terminal Observe Harness`, `Grid Buffer Math`, `Adapter Session Resume`, `Status Chrome Display`, `Vec`, `Option`, `collapsed row spans`, `String`, `open help`, `Workspace`, `PaneSpec`, `elapsed`?**
  _High betweenness centrality (0.072) - this node is a cross-community bridge._
- **Why does `AppEvent` connect `Rust Core Types` to `spawn`, `CLI State & Config`, `Focus & Attention Flow`, `pty rs`, `Pane Registry & Promotion`, `Roster & Copy State`, `Control Dispatch API`, `Float & Seam Dragging`, `Workspace`?**
  _High betweenness centrality (0.064) - this node is a cross-community bridge._
- **What connects `$schema`, `.opencode/plugins/graphify.js`, `roost` to the rest of the system?**
  _140 weakly-connected nodes found - possible documentation gaps or missing edges._
- **Should `Pane Behavior Specs` be split into smaller, more focused modules?**
  _Cohesion score 0.0345965847775027 - nodes in this community are weakly interconnected._
- **Should `Rust Core Types` be split into smaller, more focused modules?**
  _Cohesion score 0.05832258064516129 - nodes in this community are weakly interconnected._