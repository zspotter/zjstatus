# zjstatus Renderer Architecture Notes

This document explains how zjstatus's rendering pipeline works, with focus on configuration parsing, update cycles, widget execution, and color handling.

## Table of Contents

1. [Lifecycle Overview](#lifecycle-overview)
2. [Configuration Parsing](#configuration-parsing)
3. [Rendering Pipeline](#rendering-pipeline)
4. [Widget Execution](#widget-execution)
5. [Cache System](#cache-system)
6. [Color Handling](#color-handling)
7. [Future Work: Dynamic Colors](#future-work-dynamic-colors)

---

## Lifecycle Overview

### Plugin Initialization (`src/bin/zjstatus.rs:55-104`)

```
load()
├─> Request permissions (ReadApplicationState, ChangeApplicationState, RunCommands)
├─> Subscribe to events (Mouse, ModeUpdate, PaneUpdate, TabUpdate, etc.)
├─> ModuleConfig::new(&configuration)  [PARSE CONFIG ONCE]
├─> register_widgets(&configuration)   [CREATE WIDGET INSTANCES ONCE]
└─> Initialize ZellijState
```

**Key Point**: Configuration parsing and widget creation happen **exactly once** during plugin initialization. Format strings, colors, and widget configurations are parsed at startup and never re-parsed during runtime.

### Event Loop

```
Event arrives from Zellij
    ↓
update(Event)
    ↓
handle_event() → Updates state, sets cache_mask
    ↓
returns should_render = true/false
    ↓
render() → Outputs formatted status bar
```

---

## Configuration Parsing

### When Config Gets Parsed

**Timing**: Once at plugin load time in `ModuleConfig::new()` (`src/config.rs:90-170`)

**What Gets Parsed**:
1. **Format strings** (`format_left`, `format_center`, `format_right`)
   - Split by `#[` delimiter into `FormattedPart` structs
   - Each part contains: text content, fg/bg/underline colors, text effects
   - Parsing happens in `parts_from_config()` (`src/config.rs:512-526`)

2. **Inline format syntax** (`#[fg=color,bg=color,bold]content`)
   - Extracted in `FormattedPart::from_format_string()` (`src/render.rs:77-122`)
   - Colors parsed via `parse_color()` (cached, size 100)
   - Text effects: bold, italic, underscore, blink, dim, strikethrough, etc.

3. **Widget configurations**
   - Each widget extracts its config keys during registration
   - Example: `command_foo_command`, `command_foo_interval`, etc.
   - Widget configs stored in widget instances

4. **Other settings**
   - `format_space`, `format_precedence`, `format_hide_on_overlength`
   - Frame hiding options, border config

**Result**: `ModuleConfig` contains:
- Pre-parsed `Vec<FormattedPart>` for left/center/right sections
- All colors resolved to internal representation (but aliases still use lookup)
- Widget instances with their configs baked in

---

## Rendering Pipeline

### Render Trigger Flow

The plugin re-renders when `handle_event()` returns `true`:

| Event Type | State Update | Cache Mask | Triggers Render |
|------------|-------------|------------|-----------------|
| `ModeUpdate` | Updates `state.mode_info` | `UpdateEventMask::Mode` (0b01) | Yes |
| `TabUpdate` | Updates `state.tabs` | `UpdateEventMask::Tab` (0b11) | Yes |
| `PaneUpdate` | Updates `state.tabs` | `UpdateEventMask::Tab` (0b11) | Maybe (checks frame hiding) |
| `SessionUpdate` | Updates `state.sessions` | `UpdateEventMask::Session` (0b101) | Yes |
| `RunCommandResult` | Stores command output | `UpdateEventMask::Command` (0b1000) | Yes |
| `Mouse` | Handles clicks | None | No |

### Render Function (`src/bin/zjstatus.rs:153-174`)

```rust
fn render(&mut self, _rows: usize, cols: usize) {
    self.state.cols = cols;
    let output = self.module_config.render_bar(state, widget_map);
    print!("{}", output);
}
```

Called by Zellij when the status bar needs updating.

### Bar Rendering Process (`src/config.rs:327-420`)

```
render_bar()
├─> Fold left/center/right parts into strings
│   └─> For each FormattedPart: format_string_with_widgets()
│       ├─> Check cache (uses cache_mask system)
│       ├─> Process widgets if cache miss
│       ├─> Apply colors & styles via format_string()
│       └─> Cache result
├─> Handle overlength trimming (if enabled)
├─> Add border (if enabled)
└─> Calculate spacing between sections
```

---

## Widget Execution

### Widget Trait (`src/widgets/widget.rs`)

```rust
pub trait Widget {
    fn process(&self, name: &str, state: &ZellijState) -> String;
    fn process_click(&self, name: &str, state: &ZellijState, pos: usize);
}
```

### When Widgets Run

Widgets execute in `format_string_with_widgets()` (`src/render.rs:183-244`):

```rust
// Check if we can use cached content
if cache_mask & state.cache_mask == 0 && !cache.is_empty() {
    return cache.clone();  // Skip widget execution
}

// Cache miss - must execute widget
for widget_match in widget_regex.find_iter(&fmt_part.content) {
    let output = widget.process(widget_name, state);
    // ... format and append output
}
```

**Key Point**: Widgets only execute when their associated event type has occurred since last render.

### Widget Cache Masks (`src/config.rs:58-70`)

| Widget | Cache Mask | Re-runs When |
|--------|-----------|--------------|
| `command` | `Always` (0b10000000) | Every render |
| `datetime` | `Always` | Every render |
| `mode` | `Mode` (0b00000001) | Mode changes |
| `session` | `Mode` | Mode changes |
| `swap_layout` | `Tab` (0b00000011) | Tab/pane changes |
| `tabs` | `Tab` | Tab/pane changes |
| `pipe` | `Always` | Every render |
| `notifications` | `Always` | Every render |

### Widget-Specific Execution Examples

**DateTimeWidget** (`src/widgets/datetime.rs:57-106`):
- Runs every render (cache mask = Always)
- Formats current time with timezone
- No state dependencies

**CommandWidget** (`src/widgets/command.rs:175-219`):
- Checks if interval elapsed since last run
- If `interval=0`, runs only once
- Executes via `run_command()` (async)
- Result retrieved from state in next render cycle

**TabsWidget** (`src/widgets/tabs.rs`):
- Only processes when Tab events occur
- Iterates `state.tabs` to format tab list
- Expensive operation, benefits from caching

---

## Cache System

### Two-Level Caching

1. **FormattedPart Cache** (`src/render.rs:183-244`)
   - Each `FormattedPart` stores its last rendered output
   - Invalidated by `cache_mask & state.cache_mask` check
   - Prevents widget re-execution

2. **Function Result Caches**
   - `formatted_part_from_string_cached()` (size 100) - format string parsing
   - `parse_color_cached()` (size 100) - color parsing

### Cache Mask System

```
Widget cache mask:    0b00000011  (Tab)
State cache mask:     0b00000001  (Mode changed)
Bitwise AND:          0b00000001  (Non-zero = cache invalid)
```

- Each `FormattedPart` gets a mask based on widgets it contains
- `state.cache_mask` set by events (Mode, Tab, Session, Command)
- If masks intersect (non-zero AND), cache is invalidated

### Cache Mask Computation (`src/render.rs:274-292`)

```rust
fn cache_mask_from_content(content: &str) -> UpdateEventMask {
    let mut mask = UpdateEventMask::empty();
    for cap in widget_regex.captures_iter(content) {
        mask |= widget_cache_mask(&cap[1]);
    }
    mask
}
```

Scans format string for widgets, combines their masks with bitwise OR.

**Note for Dynamic Colors**: When Zellij color support is implemented, `FormattedPart::from_format_string()` should also check for Zellij color names in the fg/bg/us fields and add `UpdateEventMask::Mode` to the cache mask if any are found. This ensures the part is re-rendered when mode (and thus colors) change.

---

## Color Handling

### Current Color System

**Parse Time** (`src/render.rs:310-351`):

Colors parsed once at startup via `parse_color()`:

1. **Aliases**: `$alias_name` → looks up `color_alias_name` in config
2. **Hex RGB**: `#RRGGBB` → `RgbColor(r, g, b)`
3. **Named**: `red`, `blue`, `bright_cyan`, etc. → `AnsiColor`
4. **256-color**: `0-255` → `Ansi256Color(n)`

**Render Time** (`src/render.rs:166-181`):

Colors applied in `format_string()`:
```rust
fn format_string(text: &str, fmt_part: &FormattedPart) -> String {
    let mut style = anstyle::Style::new();
    if let Some(color) = &fmt_part.fg { style = style.fg_color(color); }
    if let Some(color) = &fmt_part.bg { style = style.bg_color(color); }
    // ... apply effects
    format!("{}{}{}", anstyle::Reset, style.render(), text)
}
```

### Color Alias Resolution

**Current Implementation**:
- Alias lookup happens in `parse_color()` at config parse time
- Format string `#[fg=$blue]` → looks up `color_blue` → resolves to `#89B4FA`
- Result is cached in `FormattedPart.fg` as `RgbColor(137, 180, 250)`
- **No runtime re-resolution** - alias value frozen at startup

---

## Future Work: Dynamic Colors

### Goal

Support color names that can change values without re-parsing the config. This would enable:
- Theme switching at runtime
- Dynamic color palettes based on mode/state
- Color values from external sources (pipes, commands)

### Zellij's Built-in Color System

Zellij provides a dynamic color palette through `ModeInfo.style.colors` that updates on every `ModeUpdate` event. This allows themes to change based on mode (normal, locked, pane, tab, etc.).

**Type Definitions** (from Zellij API):

```rust
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub struct Styling {
    pub text_unselected: StyleDeclaration,
    pub text_selected: StyleDeclaration,
    pub ribbon_unselected: StyleDeclaration,
    pub ribbon_selected: StyleDeclaration,
    pub table_title: StyleDeclaration,
    pub table_cell_unselected: StyleDeclaration,
    pub table_cell_selected: StyleDeclaration,
    pub list_unselected: StyleDeclaration,
    pub list_selected: StyleDeclaration,
    pub frame_unselected: Option<StyleDeclaration>,
    pub frame_selected: StyleDeclaration,
    pub frame_highlight: StyleDeclaration,
    pub exit_code_success: StyleDeclaration,
    pub exit_code_error: StyleDeclaration,
    pub multiplayer_user_colors: MultiplayerColors,
}

#[derive(Debug, Copy, Default, Clone, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub struct StyleDeclaration {
    pub base: PaletteColor,
    pub background: PaletteColor,
    pub emphasis_0: PaletteColor,
    pub emphasis_1: PaletteColor,
    pub emphasis_2: PaletteColor,
    pub emphasis_3: PaletteColor,
}

#[derive(Debug, Copy, Default, Clone, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub struct MultiplayerColors {
    pub player_1: PaletteColor,
    pub player_2: PaletteColor,
    pub player_3: PaletteColor,
    pub player_4: PaletteColor,
    pub player_5: PaletteColor,
    pub player_6: PaletteColor,
    pub player_7: PaletteColor,
    pub player_8: PaletteColor,
    pub player_9: PaletteColor,
    pub player_10: PaletteColor,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum PaletteColor {
    Rgb((u8, u8, u8)),
    EightBit(u8),
}
```

**Access**: `state.mode.style.colors` provides the current color palette

**Update Timing**: Updated on every `ModeUpdate` event (see `src/bin/zjstatus.rs:191-199`)

**Color Names Available**:
- Style-based: `text_selected.base`, `ribbon_unselected.emphasis_1`, `frame_highlight.background`, etc.
- Multiplayer: `player_1`, `player_2`, ..., `player_10`

**Example Usage** (proposed):
```kdl
format_left "#[fg=text_selected.base]Active#[fg=text_unselected.base] Inactive"
```

This would allow colors to automatically update when switching between modes or when the theme changes.

### Current Limitations

1. **Parse-Time Resolution**: Color aliases resolved during config parsing
   - `$blue` → `#89B4FA` happens once in `ModuleConfig::new()`
   - `FormattedPart` stores resolved `RgbColor`, not alias name
   - Zellij's dynamic colors in `mode.style.colors` are **not used** (see `src/config.rs:19` TODO)

2. **No Re-parsing**: Config never re-parsed after startup
   - Format strings frozen in `Vec<FormattedPart>`
   - Color values baked into structs

3. **Cache Invalidation**: No mechanism to invalidate color-only changes
   - Cache mask system tracks widget data, not style changes
   - Color change wouldn't trigger re-render

### Proposed Solutions

#### Option 1: Store Alias Names in FormattedPart

**Changes**:
```rust
pub struct FormattedPart {
    pub content: String,
    pub fg: Option<ColorSource>,  // Not Color directly
    pub bg: Option<ColorSource>,
    // ...
}

enum ColorSource {
    Static(Color),
    Alias(String),  // Store "$blue" as alias name
}
```

**Resolution**:
- Move color lookup from parse time to render time
- `format_string()` resolves aliases using current config state
- Requires passing config/state to color resolution

**Pros**:
- Minimal changes to parsing logic
- Aliases can change by updating config state

**Cons**:
- Performance hit (lookup on every render)
- Need to pass config context through render pipeline

#### Option 2: Separate Color Registry

**Changes**:
```rust
pub struct ColorRegistry {
    aliases: HashMap<String, Color>,
}

impl ColorRegistry {
    pub fn resolve(&self, source: &ColorSource) -> Option<Color> {
        match source {
            ColorSource::Static(c) => Some(*c),
            ColorSource::Alias(name) => self.aliases.get(name).copied(),
        }
    }

    pub fn update_alias(&mut self, name: String, color: Color) {
        self.aliases.insert(name, color);
    }
}
```

**Flow**:
- Parse format strings as before but mark alias sources
- Store registry in `ZellijState` or `ModuleConfig`
- Resolve colors at render time via registry
- Update registry through pipe messages or events

**Pros**:
- Clear separation of concerns
- Easy to update colors dynamically
- Can add cache invalidation for color changes

**Cons**:
- More complex architecture
- Need to thread registry through code

#### Option 3: Dynamic Color Cache Mask

**Changes**:
- Add `UpdateEventMask::Color` for color changes
- When colors update, set cache mask to invalidate affected parts
- Keep current parse-time resolution but allow config re-parse

**Pros**:
- Fits existing cache system
- Minimal API changes

**Cons**:
- Requires config re-parsing (expensive)
- Doesn't support truly dynamic sources

### Recommended Approach

**Modified Option 1 (Store Named Colors)** - Leverage Zellij's built-in color system:

Based on TODOs in the codebase, the implementation plan is:

1. **Phase 1**: Refactor color storage (`src/render.rs`)
   - Change `FormattedPart` to store `ColorSource` enum instead of resolved `Color`
   - `ColorSource::Static(Color)` for hex/named colors (e.g., `#RRGGBB`, `red`)
   - `ColorSource::Zellij(String)` for Zellij palette colors (e.g., `text_selected.base`)
   - `ColorSource::Alias(String)` for user aliases (e.g., `$blue`)

2. **Phase 2**: Update color parsing (`src/render.rs:parse_color()`)
   - **TODO at line 312**: "return unresolved named colors here"
   - Keep hex/RGB/256-color parsing as-is (return `ColorSource::Static`)
   - Detect Zellij color names (contain `.` or match multiplayer pattern)
   - Return `ColorSource::Zellij(name)` without resolving
   - Keep alias syntax `$name` returning `ColorSource::Alias(name)`

3. **Phase 3**: Runtime color resolution (`src/render.rs:format_string()`)
   - **TODO at line 169**: "resolve named colors here"
   - Add `state: &ZellijState` parameter to `format_string()`
   - Resolve `ColorSource` to `Color` at render time:
     ```rust
     fn resolve_color(source: &ColorSource, state: &ZellijState, config: &BTreeMap<String, String>) -> Option<Color> {
         match source {
             ColorSource::Static(color) => Some(*color),
             ColorSource::Zellij(name) => resolve_zellij_color(name, &state.mode.style.colors),
             ColorSource::Alias(name) => config.get(name).and_then(|c| parse_static_color(c)),
         }
     }
     ```
   - Convert `PaletteColor` to `anstyle::Color`

4. **Phase 4**: Cache invalidation (already works!)
   - `ModeUpdate` events already set `cache_mask = UpdateEventMask::Mode`
   - Mode changes invalidate cached FormattedParts
   - Color changes automatically trigger re-render

### Implementation Details

**Zellij Color Name Resolution**:

```rust
fn resolve_zellij_color(name: &str, styling: &Styling) -> Option<Color> {
    // Parse "text_selected.base" or "player_1"
    let parts: Vec<&str> = name.split('.').collect();

    let palette_color = match parts.as_slice() {
        ["text_selected", field] => get_style_field(&styling.text_selected, field),
        ["text_unselected", field] => get_style_field(&styling.text_unselected, field),
        ["ribbon_selected", field] => get_style_field(&styling.ribbon_selected, field),
        ["ribbon_unselected", field] => get_style_field(&styling.ribbon_unselected, field),
        // ... other StyleDeclarations
        ["player_1"] => Some(styling.multiplayer_user_colors.player_1),
        ["player_2"] => Some(styling.multiplayer_user_colors.player_2),
        // ... other players
        _ => None,
    }?;

    Some(palette_color_to_anstyle(palette_color))
}

fn get_style_field(style: &StyleDeclaration, field: &str) -> Option<PaletteColor> {
    match field {
        "base" => Some(style.base),
        "background" => Some(style.background),
        "emphasis_0" => Some(style.emphasis_0),
        "emphasis_1" => Some(style.emphasis_1),
        "emphasis_2" => Some(style.emphasis_2),
        "emphasis_3" => Some(style.emphasis_3),
        _ => None,
    }
}

fn palette_color_to_anstyle(color: PaletteColor) -> Color {
    match color {
        PaletteColor::Rgb((r, g, b)) => Color::Ansi(AnsiColor::from(RgbColor(r, g, b))),
        PaletteColor::EightBit(n) => Color::Ansi256(Ansi256Color(n)),
    }
}
```

**Files to Modify**:
- `src/render.rs`:
  - Add `ColorSource` enum
  - Update `parse_color()` at line 312 (TODO)
  - Update `format_string()` at line 169 (TODO) - add state parameter
  - Add Zellij color resolution functions
- `src/config.rs`:
  - Thread `ZellijState` to `format_string_with_widgets()` (already has access)
  - Remove TODO at line 19 once implemented

**Compatibility**:
- Static colors (hex, named, 256) work exactly as before
- User aliases `$name` continue to work
- New Zellij color names: `text_selected.base`, `player_1`, etc.
- Mixed usage supported: `#[fg=#FF0000,bg=text_selected.background]`

**Cache Behavior**:
- FormattedParts with Zellij colors get `Mode` cache mask
- Re-render automatically triggered on mode changes
- No additional cache invalidation needed

**Testing**:
- Verify static colors still work (hex, named, 256, aliases)
- Test Zellij color resolution (all StyleDeclaration fields, multiplayer colors)
- Test color updates on mode changes
- Benchmark: should be negligible (only runs on cache miss)
- Test fallback behavior for invalid color names

---

## Summary

### Current Architecture

1. **Config parsing**: Once at startup, never re-parsed
2. **Widget execution**: Event-driven with cache mask system
3. **Rendering**: Pull-based, triggered by Zellij on state changes
4. **Colors**: Resolved at parse time, frozen in FormattedPart (static only)
5. **Zellij colors**: Available in `state.mode.style.colors` but not currently used

### Key Insights

- **Efficiency**: Cache mask system prevents unnecessary widget execution
- **Simplicity**: One-time parsing keeps runtime overhead low
- **Limitation**: No runtime config changes, including colors
- **Opportunity**: Zellij provides dynamic color palette that updates with mode changes

### Path Forward

To support Zellij's dynamic colors:
1. Store color names instead of resolved values in FormattedPart (ColorSource enum)
2. Resolve Zellij color names at render time from `state.mode.style.colors`
3. Keep user aliases and static colors working as before
4. Cache mask system already handles Mode updates - no changes needed
5. Minimal performance impact (resolution only on cache miss)

**Implementation locations**:
- `src/render.rs:312` - parse_color() TODO
- `src/render.rs:169` - format_string() TODO
- `src/config.rs:19` - mode.style.colors TODO
