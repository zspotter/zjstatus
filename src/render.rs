use cached::{proc_macro::cached, SizedCache};
use lazy_static::lazy_static;
use std::{collections::BTreeMap, sync::Arc};

use anstyle::{Ansi256Color, AnsiColor, Color, RgbColor, Style};
use regex::Regex;
use zellij_tile::prelude::{bail, PaletteColor, StyleDeclaration, Styling};

use crate::{
    config::{event_mask_from_widget_name, UpdateEventMask, ZellijState},
    widgets::widget::Widget,
};

lazy_static! {
    static ref WIDGET_REGEX: Regex = Regex::new("(\\{[a-z_0-9]+\\})").unwrap();
}

/// Represents a color that may need runtime resolution
#[derive(Clone, Debug, PartialEq)]
pub enum ColorSource {
    /// Static color resolved at parse time (hex, RGB, named ANSI colors, user aliases)
    Static(Color),
    /// Zellij palette color resolved at render time from mode.style.colors
    /// Examples: "text_selected.base", "ribbon_unselected.emphasis_1", "player_1"
    Zellij(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct FormattedPart {
    pub fg: Option<ColorSource>,
    pub bg: Option<ColorSource>,
    pub us: Option<ColorSource>,
    pub effects: anstyle::Effects,
    pub bold: bool,
    pub italic: bool,
    pub underscore: bool,
    pub reverse: bool,
    pub blink: bool,
    pub hidden: bool,
    pub dimmed: bool,
    pub strikethrough: bool,
    pub double_underscore: bool,
    pub curly_underscore: bool,
    pub dotted_underscore: bool,
    pub dashed_underscore: bool,
    pub content: String,
    pub cache_mask: u8,
    pub cached_content: String,
    pub cache: BTreeMap<String, String>,
}

#[cached(
    ty = "SizedCache<String, FormattedPart>",
    create = "{ SizedCache::with_size(100) }",
    convert = r#"{ (format.to_owned()) }"#
)]
pub fn formatted_part_from_string_cached(
    format: &str,
    config: &BTreeMap<String, String>,
) -> FormattedPart {
    FormattedPart::from_format_string(format, config)
}

#[cached(
    ty = "SizedCache<String, Vec<FormattedPart>>",
    create = "{ SizedCache::with_size(100) }",
    convert = r#"{ (config_string.to_owned()) }"#
)]
pub fn formatted_parts_from_string_cached(
    config_string: &str,
    config: &BTreeMap<String, String>,
) -> Vec<FormattedPart> {
    FormattedPart::multiple_from_format_string(config_string, config)
}

impl FormattedPart {
    pub fn multiple_from_format_string(
        config_string: &str,
        config: &BTreeMap<String, String>,
    ) -> Vec<Self> {
        config_string
            .split("#[")
            .map(|s| FormattedPart::from_format_string(s, config))
            .collect()
    }

    pub fn from_format_string(format: &str, config: &BTreeMap<String, String>) -> Self {
        let format = match format.starts_with("#[") {
            true => format.strip_prefix("#[").unwrap(),
            false => format,
        };

        let mut result = FormattedPart {
            cache_mask: cache_mask_from_content(format),
            ..Default::default()
        };

        let mut format_content_split = format.split(']').collect::<Vec<&str>>();

        if format_content_split.len() == 1 {
            format.clone_into(&mut result.content);

            return result;
        }

        let parts = format_content_split[0].split(',');

        format_content_split.remove(0);
        result.content = format_content_split.join("]");

        for part in parts {
            if part.starts_with("fg=") {
                result.fg = parse_color(part.strip_prefix("fg=").unwrap(), config);
            }

            if part.starts_with("bg=") {
                result.bg = parse_color(part.strip_prefix("bg=").unwrap(), config);
            }

            if part.starts_with("us=") {
                result.us = parse_color(part.strip_prefix("us=").unwrap(), config);
            }

            if part.eq("reverse") {
                result.reverse = true;
            }

            result.parse_and_set_effect(part);
        }

        // If any colors are Zellij colors, add Mode mask so they update when mode changes
        let has_zellij_colors = [&result.fg, &result.bg, &result.us]
            .iter()
            .any(|color_opt| {
                matches!(color_opt, Some(ColorSource::Zellij(_)))
            });

        if has_zellij_colors {
            result.cache_mask |= UpdateEventMask::Mode as u8;
        }

        result
    }

    fn parse_and_set_effect(&mut self, part: &str) {
        match part {
            "bold" => {
                self.effects |= anstyle::Effects::BOLD;
            }
            "italic" | "italics" => {
                self.effects |= anstyle::Effects::ITALIC;
            }
            "underscore" => {
                self.effects |= anstyle::Effects::UNDERLINE;
            }
            "blink" => {
                self.effects |= anstyle::Effects::BLINK;
            }
            "hidden" => {
                self.effects |= anstyle::Effects::HIDDEN;
            }
            "dim" => {
                self.effects |= anstyle::Effects::DIMMED;
            }
            "strikethrough" => {
                self.effects |= anstyle::Effects::STRIKETHROUGH;
            }
            "double-underscore" => {
                self.effects |= anstyle::Effects::DOUBLE_UNDERLINE;
            }
            "curly-underscore" => {
                self.effects |= anstyle::Effects::CURLY_UNDERLINE;
            }
            "dotted-underscore" => {
                self.effects |= anstyle::Effects::DOTTED_UNDERLINE;
            }
            "dashed-underscore" => {
                self.effects |= anstyle::Effects::DASHED_UNDERLINE;
            }
            "reverse" => {
                self.effects |= anstyle::Effects::INVERT;
            }
            _ => {}
        }
    }

    pub fn format_string(&self, text: &str, state: &ZellijState) -> String {
        let mut style = Style::new();

        // Resolve colors at render time
        if let Some(fg_source) = &self.fg {
            if let Some(fg) = resolve_color_source(fg_source, state) {
                style = style.fg_color(Some(fg));
            }
        }

        if let Some(bg_source) = &self.bg {
            if let Some(bg) = resolve_color_source(bg_source, state) {
                style = style.bg_color(Some(bg));
            }
        }

        if let Some(us_source) = &self.us {
            if let Some(us) = resolve_color_source(us_source, state) {
                style = style.underline_color(Some(us));
            }
        }

        style = style.effects(self.effects);

        format!(
            "{}{}{}{}",
            style.render_reset(),
            style.render(),
            text,
            style.render_reset()
        )
    }

    #[tracing::instrument(skip_all)]
    pub fn format_string_with_widgets(
        &mut self,
        widgets: &BTreeMap<String, Arc<dyn Widget>>,
        state: &ZellijState,
    ) -> String {
        let skip_cache = self.cache_mask & UpdateEventMask::Always as u8 != 0;

        if !skip_cache && self.cache_mask & state.cache_mask == 0 && !self.cache.is_empty() {
            tracing::debug!(msg = "hit", typ = "format_string", format = self.content);
            return self.cached_content.to_owned();
        }
        tracing::debug!(msg = "miss", typ = "format_string", format = self.content);

        let mut output = self.content.clone();

        for widget in WIDGET_REGEX.captures_iter(&self.content) {
            let match_name = widget.get(0).unwrap().as_str();
            let widget_key = match_name.trim_matches(|c| c == '{' || c == '}');
            let mut widget_key_name = widget_key;

            if widget_key.starts_with("command_") {
                widget_key_name = "command";
            }

            if widget_key.starts_with("pipe_") {
                widget_key_name = "pipe";
            }

            let widget_mask = event_mask_from_widget_name(widget_key_name);
            let skip_widget_cache = widget_mask & UpdateEventMask::Always as u8 != 0;
            if !skip_widget_cache && widget_mask & state.cache_mask == 0 {
                if let Some(res) = self.cache.get(widget_key) {
                    tracing::debug!(msg = "hit", typ = "widget", widget = widget_key);
                    output = output.replace(match_name, res);
                    continue;
                }
            }

            tracing::debug!(
                msg = "miss",
                typ = "widget",
                widget = widget_key,
                mask = widget_mask & state.cache_mask,
                skip_cache = skip_cache,
            );

            let result = match widgets.get(widget_key_name) {
                Some(widget) => widget.process(widget_key, state),
                None => "Use of uninitialized widget".to_owned(),
            };

            self.cache.insert(widget_key.to_owned(), result.to_owned());

            output = output.replace(match_name, &result);
        }

        let res = self.format_string(&output, state);
        self.cached_content.clone_from(&res);

        res
    }
}

impl Default for FormattedPart {
    fn default() -> Self {
        Self {
            fg: None,
            bg: None,
            us: None,
            effects: anstyle::Effects::new(),
            bold: false,
            italic: false,
            underscore: false,
            reverse: false,
            blink: false,
            hidden: false,
            dimmed: false,
            strikethrough: false,
            double_underscore: false,
            curly_underscore: false,
            dotted_underscore: false,
            dashed_underscore: false,
            content: "".to_owned(),
            cache_mask: 0,
            cached_content: "".to_owned(),
            cache: BTreeMap::new(),
        }
    }
}

fn cache_mask_from_content(content: &str) -> u8 {
    let mut output = 0;
    for widget in WIDGET_REGEX.captures_iter(content) {
        let match_name = widget.get(0).unwrap().as_str();
        let widget_key = match_name.trim_matches(|c| c == '{' || c == '}');
        let mut widget_key_name = widget_key;

        if widget_key.starts_with("command_") {
            widget_key_name = "command";
        }

        if widget_key.starts_with("pipe_") {
            widget_key_name = "pipe";
        }

        output |= event_mask_from_widget_name(widget_key_name);
    }
    output
}

fn hex_to_rgb(s: &str) -> anyhow::Result<Vec<u8>> {
    if s.len() != 6 {
        bail!("wrong hex color length");
    }

    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(anyhow::Error::from))
        .collect()
}

/// Checks if a color name matches Zellij's palette color naming patterns
fn is_zellij_color_name(name: &str) -> bool {
    // Check for player colors: "player_1" through "player_10"
    if name.starts_with("player_") {
        if let Some(num_str) = name.strip_prefix("player_") {
            if let Ok(num) = num_str.parse::<u8>() {
                return (1..=10).contains(&num);
            }
        }
        return false;
    }

    // Check for StyleDeclaration colors: "category.field"
    let parts: Vec<&str> = name.split('.').collect();
    if parts.len() != 2 {
        return false;
    }

    let category = parts[0];
    let field = parts[1];

    // Valid categories (StyleDeclaration fields in Styling struct)
    let valid_categories = [
        "text_unselected",
        "text_selected",
        "ribbon_unselected",
        "ribbon_selected",
        "table_title",
        "table_cell_unselected",
        "table_cell_selected",
        "list_unselected",
        "list_selected",
        "frame_unselected",
        "frame_selected",
        "frame_highlight",
        "exit_code_success",
        "exit_code_error",
    ];

    // Valid fields (PaletteColor fields in StyleDeclaration)
    let valid_fields = [
        "base",
        "background",
        "emphasis_0",
        "emphasis_1",
        "emphasis_2",
        "emphasis_3",
    ];

    valid_categories.contains(&category) && valid_fields.contains(&field)
}

#[cached(
    ty = "SizedCache<String, Option<ColorSource>>",
    create = "{ SizedCache::with_size(100) }",
    convert = r#"{ (color.to_owned()) }"#
)]
fn parse_color(color: &str, config: &BTreeMap<String, String>) -> Option<ColorSource> {
    // Handle user-defined aliases - resolve at parse time since config doesn't change
    if color.starts_with('$') {
        let alias_name = color.strip_prefix('$').unwrap();
        let alias_value = config.get(&format!("color_{}", alias_name))?;
        // Recursively parse the alias value as a static color
        return parse_static_color(alias_value).map(ColorSource::Static);
    }

    // Check if this looks like a Zellij color name - keep dynamic for runtime resolution
    // Patterns: "text_selected.base", "player_1", etc.
    if is_zellij_color_name(color) {
        return Some(ColorSource::Zellij(color.to_owned()));
    }

    // Parse static hex colors
    if color.starts_with('#') {
        let rgb = match hex_to_rgb(color.strip_prefix('#').unwrap()) {
            Ok(rgb) => rgb,
            Err(_) => return None,
        };

        if rgb.len() != 3 {
            return None;
        }

        return Some(ColorSource::Static(
            RgbColor(
                *rgb.first().unwrap(),
                *rgb.get(1).unwrap(),
                *rgb.get(2).unwrap(),
            )
            .into(),
        ));
    }

    // Parse named ANSI colors
    if let Some(ansi_color) = color_by_name(color) {
        return Some(ColorSource::Static(ansi_color.into()));
    }

    // Parse 256-color palette (with optional "colour" prefix)
    let mut color_str = color;
    if color.starts_with("colour") {
        color_str = color.strip_prefix("colour").unwrap();
    }

    if let Ok(result) = color_str.parse::<u8>() {
        return Some(ColorSource::Static(Ansi256Color(result).into()));
    }

    None
}

fn color_by_name(color: &str) -> Option<AnsiColor> {
    match color {
        "black" => Some(AnsiColor::Black),
        "red" => Some(AnsiColor::Red),
        "green" => Some(AnsiColor::Green),
        "yellow" => Some(AnsiColor::Yellow),
        "blue" => Some(AnsiColor::Blue),
        "magenta" => Some(AnsiColor::Magenta),
        "cyan" => Some(AnsiColor::Cyan),
        "white" => Some(AnsiColor::White),
        "bright_black" => Some(AnsiColor::BrightBlack),
        "bright_red" => Some(AnsiColor::BrightRed),
        "bright_green" => Some(AnsiColor::BrightGreen),
        "bright_yellow" => Some(AnsiColor::BrightYellow),
        "bright_blue" => Some(AnsiColor::BrightBlue),
        "bright_magenta" => Some(AnsiColor::BrightMagenta),
        "bright_cyan" => Some(AnsiColor::BrightCyan),
        "bright_white" => Some(AnsiColor::BrightWhite),
        "default" => None,
        _ => None,
    }
}

/// Resolves a ColorSource to an actual Color at render time
fn resolve_color_source(source: &ColorSource, state: &ZellijState) -> Option<Color> {
    match source {
        ColorSource::Static(color) => Some(*color),
        ColorSource::Zellij(name) => resolve_zellij_color(name, &state.mode.style.colors),
    }
}

/// Parses a color string as a static color (no aliases or Zellij colors)
fn parse_static_color(color: &str) -> Option<Color> {
    // Parse hex colors
    if color.starts_with('#') {
        let rgb = hex_to_rgb(color.strip_prefix('#').unwrap()).ok()?;
        if rgb.len() != 3 {
            return None;
        }
        return Some(
            RgbColor(*rgb.first()?, *rgb.get(1)?, *rgb.get(2)?).into(),
        );
    }

    // Parse named ANSI colors
    if let Some(ansi_color) = color_by_name(color) {
        return Some(ansi_color.into());
    }

    // Parse 256-color palette
    let mut color_str = color;
    if color.starts_with("colour") {
        color_str = color.strip_prefix("colour").unwrap();
    }

    if let Ok(result) = color_str.parse::<u8>() {
        return Some(Ansi256Color(result).into());
    }

    None
}

/// Resolves a Zellij color name from the Styling palette
fn resolve_zellij_color(name: &str, styling: &Styling) -> Option<Color> {
    // Handle player colors
    if name.starts_with("player_") {
        let num_str = name.strip_prefix("player_")?;
        let num = num_str.parse::<u8>().ok()?;
        let palette_color = match num {
            1 => styling.multiplayer_user_colors.player_1,
            2 => styling.multiplayer_user_colors.player_2,
            3 => styling.multiplayer_user_colors.player_3,
            4 => styling.multiplayer_user_colors.player_4,
            5 => styling.multiplayer_user_colors.player_5,
            6 => styling.multiplayer_user_colors.player_6,
            7 => styling.multiplayer_user_colors.player_7,
            8 => styling.multiplayer_user_colors.player_8,
            9 => styling.multiplayer_user_colors.player_9,
            10 => styling.multiplayer_user_colors.player_10,
            _ => return None,
        };
        return Some(palette_color_to_anstyle(palette_color));
    }

    // Handle StyleDeclaration colors
    let parts: Vec<&str> = name.split('.').collect();
    if parts.len() != 2 {
        return None;
    }

    let category = parts[0];
    let field = parts[1];

    let style_decl = match category {
        "text_unselected" => &styling.text_unselected,
        "text_selected" => &styling.text_selected,
        "ribbon_unselected" => &styling.ribbon_unselected,
        "ribbon_selected" => &styling.ribbon_selected,
        "table_title" => &styling.table_title,
        "table_cell_unselected" => &styling.table_cell_unselected,
        "table_cell_selected" => &styling.table_cell_selected,
        "list_unselected" => &styling.list_unselected,
        "list_selected" => &styling.list_selected,
        "frame_unselected" => styling.frame_unselected.as_ref()?,
        "frame_selected" => &styling.frame_selected,
        "frame_highlight" => &styling.frame_highlight,
        "exit_code_success" => &styling.exit_code_success,
        "exit_code_error" => &styling.exit_code_error,
        _ => return None,
    };

    let palette_color = get_style_field(style_decl, field)?;
    Some(palette_color_to_anstyle(palette_color))
}

/// Extracts a specific field from a StyleDeclaration
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

/// Converts a Zellij PaletteColor to an anstyle Color
fn palette_color_to_anstyle(palette_color: PaletteColor) -> Color {
    match palette_color {
        PaletteColor::Rgb((r, g, b)) => RgbColor(r, g, b).into(),
        PaletteColor::EightBit(n) => Ansi256Color(n).into(),
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_hex_to_rgb() {
        let result = hex_to_rgb("010203");
        let expected = Vec::from([1, 2, 3]);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), expected);
    }

    #[test]
    fn test_hex_to_rgb_with_invalid_input() {
        let result = hex_to_rgb("#010203");
        assert!(result.is_err());

        let result = hex_to_rgb(" 010203");
        assert!(result.is_err());

        let result = hex_to_rgb("010");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_color() {
        let mut config: BTreeMap<String, String> = BTreeMap::new();
        config.insert("color_green".to_owned(), "#00ff00".to_owned());

        let result = parse_color("#010203", &config);
        let expected = RgbColor(1, 2, 3);
        assert_eq!(result, Some(expected.into()));

        let result = parse_color("255", &config);
        let expected = Ansi256Color(255);
        assert_eq!(result, Some(expected.into()));

        let result = parse_color("365", &config);
        assert_eq!(result, None);

        let result = parse_color("#365", &config);
        assert_eq!(result, None);

        let result = parse_color("$green", &config);
        let expected = RgbColor(0, 255, 0);
        assert_eq!(result, Some(expected.into()));

        let result = parse_color("$blue", &config);
        assert_eq!(result, None);
    }
}
