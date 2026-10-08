// Modified for RCode. See NOTICE.
export type RCodeGhosttyCursor = {
  readonly row: number;
  readonly column: number;
  readonly visible: boolean;
  readonly style: "block" | "underline" | "bar";
  readonly blinking: boolean;
  readonly wideTail: boolean;
  readonly colorRgba: number;
};

export type RCodeGhosttyRenderState = {
  readonly rows: number;
  readonly cols: number;
  readonly cellCount: number;
  readonly codepoints: Uint32Array;
  readonly contentTags: Uint8Array;
  readonly widths: Uint8Array;
  readonly cellFlags: Uint16Array;
  readonly styleFlags: Uint16Array;
  readonly underlineStyles: Uint8Array;
  readonly linkIds: Uint32Array;
  readonly foregroundRgba: Uint8Array;
  readonly backgroundRgba: Uint8Array;
  readonly underlineRgba: Uint8Array;
  readonly graphemeOffsets: Uint32Array;
  readonly graphemeLengths: Uint32Array;
  readonly graphemeCodepoints: Uint32Array;
  readonly selectionStarts: Int16Array;
  readonly selectionEnds: Int16Array;
  readonly rowWrapped: Uint8Array;
  readonly dirtyRows: Uint8Array;
  readonly fullDamage: boolean;
  readonly linkOffsets: Uint32Array;
  readonly linkLengths: Uint32Array;
  readonly linkBytes: Uint8Array;
  readonly cursor: RCodeGhosttyCursor;
};

export type RCodeGhosttyKeyEvent = {
  readonly action: number;
  readonly key: number;
  readonly mods: number;
  readonly consumedMods?: number;
  readonly composing?: boolean;
  readonly utf8?: string;
  readonly unshiftedCodepoint?: number;
};

export type RCodeGhosttyTerminalOptions = {
  readonly maxScrollbackBytes?: number;
  readonly maxScrollbackLines?: number;
};

export type RCodeGhosttyTerminalResourceStats = {
  readonly cellCapacity: number;
  readonly rowCapacity: number;
  readonly renderStateResets: number;
};

export type RCodeGhosttySearchStatus = {
  readonly active: boolean;
  readonly pending: boolean;
  readonly complete: boolean;
  readonly generation: number;
  readonly totalMatches: number;
  readonly selectedIndex: number;
};

export type RCodeGhosttySearchViewportSpan = {
  readonly row: number;
  readonly startColumn: number;
  readonly endColumn: number;
  readonly selected: boolean;
};

export type RCodeGhosttySelection = {
  readonly anchor: { readonly line: number; readonly column: number };
  readonly focus: { readonly line: number; readonly column: number };
  readonly rectangular: boolean;
};

export type RCodeGhosttyLoadOptions = {
  readonly log?: (message: string) => void;
};

export type RCodeGhosttyWasmExports = WebAssembly.Exports & {
  readonly memory: WebAssembly.Memory;
  readonly rcode_create_with_limits: (
    cols: number,
    rows: number,
    maxScrollbackBytes: number,
    maxScrollbackLines: number,
  ) => number;
  readonly rcode_destroy: (handle: number) => void;
  readonly rcode_write: (handle: number, ptr: number, len: number) => number;
  readonly rcode_resize: (handle: number, cols: number, rows: number) => number;
  readonly rcode_set_pixel_size: (
    handle: number,
    widthPx: number,
    heightPx: number,
  ) => number;
  readonly rcode_render_update: (handle: number) => number;
  readonly rcode_render_release: (handle: number) => void;
  readonly rcode_render_compact: (handle: number) => number;
  readonly rcode_cell_capacity: (handle: number) => number;
  readonly rcode_row_capacity: (handle: number) => number;
  readonly rcode_render_reset_count: (handle: number) => number;
  readonly rcode_alloc: (len: number) => number;
  readonly rcode_free: (ptr: number, len: number) => void;
  readonly rcode_output_ptr: (handle: number) => number;
  readonly rcode_output_len: (handle: number) => number;
  readonly rcode_output_consume: (handle: number, len: number) => number;
  readonly rcode_events_ptr: (handle: number) => number;
  readonly rcode_events_len: (handle: number) => number;
  readonly rcode_events_consume: (handle: number, len: number) => number;
  readonly rcode_take_dropped_events: (handle: number) => number;
  readonly rcode_scroll_viewport: (handle: number, delta: number) => number;
  readonly rcode_scroll_viewport_to: (handle: number, row: number) => number;
  readonly rcode_scrollbar_total: (handle: number) => number;
  readonly rcode_scrollbar_offset: (handle: number) => number;
  readonly rcode_scrollbar_len: (handle: number) => number;
  readonly rcode_active_cursor_y: (handle: number) => number;
  readonly rcode_selection_set: (
    handle: number,
    startLine: number,
    startColumn: number,
    endLine: number,
    endColumn: number,
    rectangular: number,
  ) => number;
  readonly rcode_semantic_markers_enable: (handle: number, enabled: number) => void;
  readonly rcode_semantic_marker_column: (handle: number, id: number) => number;
  readonly rcode_semantic_marker_line: (handle: number, id: number) => number;
  readonly rcode_semantic_marker_count: (handle: number) => number;
  readonly rcode_text_range_prepare: (handle: number, startLine: number, startCol: number, endLine: number, endCol: number) => number;
  readonly rcode_selection_clear: (handle: number) => number;
  readonly rcode_selection_active: (handle: number) => number;
  readonly rcode_selection_start_line: (handle: number) => number;
  readonly rcode_selection_start_col: (handle: number) => number;
  readonly rcode_selection_end_line: (handle: number) => number;
  readonly rcode_selection_end_col: (handle: number) => number;
  readonly rcode_selection_rectangular: (handle: number) => number;
  readonly rcode_selection_text_prepare: (handle: number) => number;
  readonly rcode_selection_text_ptr: (handle: number) => number;
  readonly rcode_selection_text_len: (handle: number) => number;
  readonly rcode_set_default_colors: (
    handle: number,
    foreground: number,
    background: number,
    cursor: number,
  ) => number;
  readonly rcode_set_palette: (
    handle: number,
    ptr: number,
    count: number,
  ) => number;
  readonly rcode_reset_palette: (handle: number) => number;
  readonly rcode_mode: (
    handle: number,
    value: number,
    ansi: number,
  ) => number;
  readonly rcode_mode_bits: (handle: number) => number;
  readonly rcode_set_cursor_options: (
    handle: number,
    style: number,
    blinking: number,
  ) => number;
  readonly rcode_encode_key: (
    handle: number,
    action: number,
    key: number,
    mods: number,
    consumedMods: number,
    composing: number,
    unshiftedCodepoint: number,
    utf8Pointer: number,
    utf8Length: number,
  ) => number;
  readonly rcode_key_output_ptr: (handle: number) => number;
  readonly rcode_key_output_len: (handle: number) => number;
  readonly rcode_search_set_query: (
    handle: number,
    ptr: number,
    len: number,
  ) => number;
  readonly rcode_search_clear: (handle: number) => number;
  readonly rcode_search_step: (handle: number, budget: number) => number;
  readonly rcode_search_status_ptr: (handle: number) => number;
  readonly rcode_search_viewport_match_count: (handle: number) => number;
  readonly rcode_search_viewport_matches_ptr: (handle: number) => number;
  readonly rcode_search_select_next: (handle: number) => number;
  readonly rcode_search_select_prev: (handle: number) => number;
  readonly rcode_rows: (handle: number) => number;
  readonly rcode_cols: (handle: number) => number;
  readonly rcode_cell_codepoints_ptr: (handle: number) => number;
  readonly rcode_cell_content_tags_ptr: (handle: number) => number;
  readonly rcode_cell_wide_ptr: (handle: number) => number;
  readonly rcode_cell_flags_ptr: (handle: number) => number;
  readonly rcode_cell_style_flags_ptr: (handle: number) => number;
  readonly rcode_cell_underline_styles_ptr: (handle: number) => number;
  readonly rcode_cell_link_ids_ptr: (handle: number) => number;
  readonly rcode_cell_fg_rgba_ptr: (handle: number) => number;
  readonly rcode_cell_bg_rgba_ptr: (handle: number) => number;
  readonly rcode_cell_ul_rgba_ptr: (handle: number) => number;
  readonly rcode_cell_grapheme_offsets_ptr: (handle: number) => number;
  readonly rcode_cell_grapheme_lengths_ptr: (handle: number) => number;
  readonly rcode_grapheme_buffer_ptr: (handle: number) => number;
  readonly rcode_grapheme_buffer_len: (handle: number) => number;
  readonly rcode_row_selection_start_ptr: (handle: number) => number;
  readonly rcode_row_selection_end_ptr: (handle: number) => number;
  readonly rcode_row_wrapped_ptr: (handle: number) => number;
  readonly rcode_row_dirty_ptr: (handle: number) => number;
  readonly rcode_damage_full: (handle: number) => number;
  readonly rcode_cursor_info_ptr: (handle: number) => number;
  readonly rcode_link_offsets_ptr: (handle: number) => number;
  readonly rcode_link_lengths_ptr: (handle: number) => number;
  readonly rcode_link_buffer_ptr: (handle: number) => number;
  readonly rcode_link_count: (handle: number) => number;
  readonly rcode_link_buffer_len: (handle: number) => number;
};

export type TypedArray =
  | Uint8Array
  | Uint16Array
  | Uint32Array
  | Int16Array;

export type TypedArrayConstructor<T extends TypedArray> = {
  readonly BYTES_PER_ELEMENT: number;
  new (buffer: ArrayBuffer, byteOffset: number, length: number): T;
};

export type ViewCacheEntry<T extends TypedArray> = {
  buffer: ArrayBuffer | null;
  pointer: number;
  length: number;
  view: T | null;
};
