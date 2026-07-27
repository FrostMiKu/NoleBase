use ratatui::style::Color;

/// Catppuccin Mocha colors shared by Nole's application chrome.
pub mod catppuccin {
    use super::Color;

    pub const PINK: Color = Color::Rgb(245, 194, 231);
    pub const MAUVE: Color = Color::Rgb(203, 166, 247);
    pub const YELLOW: Color = Color::Rgb(249, 226, 175);
    pub const GREEN: Color = Color::Rgb(166, 227, 161);
    pub const TEAL: Color = Color::Rgb(148, 226, 213);
    pub const SKY: Color = Color::Rgb(137, 220, 235);
    pub const SAPPHIRE: Color = Color::Rgb(116, 199, 236);
    pub const BLUE: Color = Color::Rgb(137, 180, 250);
    pub const LAVENDER: Color = Color::Rgb(180, 190, 254);
    pub const SUBTEXT_1: Color = Color::Rgb(186, 194, 222);
    pub const SUBTEXT_0: Color = Color::Rgb(166, 173, 200);
    pub const OVERLAY_1: Color = Color::Rgb(127, 132, 156);
    pub const OVERLAY_0: Color = Color::Rgb(108, 112, 134);
    pub const SURFACE_1: Color = Color::Rgb(69, 71, 90);
    pub const SURFACE_0: Color = Color::Rgb(49, 50, 68);
    pub const BASE: Color = Color::Rgb(30, 30, 46);
    pub const MANTLE: Color = Color::Rgb(24, 24, 37);
    pub const CRUST: Color = Color::Rgb(17, 17, 27);
}

#[cfg(test)]
mod tests {
    use mbtui::Theme;

    use super::catppuccin as ctp;

    #[test]
    fn application_palette_tracks_mbtui_catppuccin_defaults() {
        let theme = Theme::default();
        assert_eq!(theme.heading_1.fg, Some(ctp::LAVENDER));
        assert_eq!(theme.code_block.bg, Some(ctp::SURFACE_0));
        assert_eq!(theme.link.fg, Some(ctp::BLUE));
        assert_eq!(theme.link.underline_color, Some(ctp::BLUE));
    }
}
