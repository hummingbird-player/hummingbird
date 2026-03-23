# Theming
Hummingbird includes a built-in default theme and can also load custom themes from its data directory.

## Theme locations

| Theme type       | Location                   |
|------------------|----------------------------|
| Built-in default | No file required           |
| Custom themes    | `<data dir>/themes/*.json` |

Platform-specific data directory locations:

| Platform | Data directory                                           |
|----------|----------------------------------------------------------|
| Linux    | `~/.local/share/hummingbird/`                            |
| macOS    | `~/Library/Application Support/org.mailliw.hummingbird/` |
| Windows  | `%appdata%\mailliw\hummingbird\data\`                    |

> [!NOTE]
> The default data directory was chanaged when Muzak was renamed to Hummingbird.
>
> If you first opened the application before the name change, your configuration files may
> be in the previous location.
>
> <details>
> <summary>Legacy (pre-Hummingbird) folder location</summary>
> <br>
>
> | Platform | Location                                                       |
> |----------|----------------------------------------------------------------|
> | Linux    | `~/.local/share/muzak/theme.json`                              |
> | macOS    | `~/Library/Application Support/me.william341.muzak/theme.json` |
> | Windows  | `%appdata%\william341\muzak\data\theme.json`                   |
>
> This can be applied to all paths - they have all been changed in the same manner.
> </details>

## Selecting a theme

Open **Settings > Interface > Theme** to choose from available themes.

Place custom theme files in the `themes/` folder and they will appear in the list.

## How theme changes work

Your theme is applied immediately when you select it, and updates automatically
when you edit or replace the theme file.

If something goes wrong:

- Broken theme files use the built-in default
- Missing theme files use the built-in default

If your colors do not appear to change, check that your theme file is valid JSON. 
In cases where it is not, a warning with an error will be printed to the console.

Colors are specified as CSS-style hex codes (`#ABCDEF`). If a color is not
specified, the color from the default theme is used.

## Example
A `theme.json` for the default theme is provided here. Note the colors may be
out of date, but an effort is made to ensure all possible fields are represented
in this example.

```json
{
  "background_primary": "#0D0E12",
  "background_secondary": "#161720",
  "background_tertiary": "#1A1D26",
  "border_color": "#202233",
  "album_art_background": "#303246",
  "text": "#E8E9F2",
  "text_secondary": "#A0A1AD",
  "text_disabled": "#5F5F71",
  "text_link": "#5279D4",
  "nav_button_hover": "#1A1C28",
  "nav_button_hover_border": "#212431",
  "nav_button_active": "#151620",
  "nav_button_active_border": "#191B27",
  "nav_button_pressed": "#1F212D",
  "nav_button_pressed_border": "#292D3F",
  "playback_button": "#00000000",
  "playback_button_hover": "#272B41",
  "playback_button_active": "#08080B",
  "playback_button_border": "#00000000",
  "playback_button_toggled": "#688CF0",
  "window_button": "#00000000",
  "window_button_hover": "#262D42",
  "window_button_active": "#0D0F14",
  "close_button": "#00000000",
  "close_button_hover": "#7E2C2C",
  "close_button_active": "#5B1D1D",
  "queue_item": "#00000000",
  "queue_item_hover": "#151621",
  "queue_item_active": "#101118",
  "queue_item_current": "#1B1C28",
  "button_primary": "#5774E7",
  "button_primary_border": "#6D85E4",
  "button_primary_hover": "#6D92FF",
  "button_primary_border_hover": "#5488FF",
  "button_primary_active": "#495F9F",
  "button_primary_border_active": "#515C8F",
  "button_primary_text": "#E0E7F7",
  "button_secondary": "#373B4E",
  "button_secondary_border": "#4F5267",
  "button_secondary_hover": "#494E67",
  "button_secondary_border_hover": "#565A77",
  "button_secondary_active": "#262636",
  "button_secondary_border_active": "#2F3244",
  "button_secondary_text": "#DDDEEC",
  "button_warning": "#97792C",
  "button_warning_border": "#C59E4F",
  "button_warning_hover": "#A98B4A",
  "button_warning_border_hover": "#C9A558",
  "button_warning_active": "#5D4B2E",
  "button_warning_border_active": "#80683F",
  "button_warning_text": "#F0EBDE",
  "button_danger": "#CD0B0B",
  "button_danger_border": "#A00808",
  "button_danger_hover": "#E80C0C",
  "button_danger_border_hover": "#CF0B0B",
  "button_danger_active": "#B70A0A",
  "button_danger_border_active": "#990707",
  "button_danger_text": "#E9D4D4",
  "slider_foreground": "#688CF0",
  "slider_background": "#38374E",
  "elevated_background": "#161820",
  "elevated_border_color": "#23253B",
  "menu_item": "#00000000",
  "menu_item_hover": "#1F2334",
  "menu_item_border_hover": "#2B2F44",
  "menu_item_active": "#0E0F15",
  "menu_item_border_active": "#1F212E",
  "modal_overlay_bg": "#00000055",
  "text_input_selection": "#01020388",
  "caret_color": "#E8E8F2",
  "palette_item_hover": "#1F2334",
  "palette_item_border_hover": "#2B2F44",
  "palette_item_active": "#0E0F15",
  "palette_item_border_active": "#1F212E",
  "scrollbar_background": "#252839",
  "scrollbar_foreground": "#616794",
  "textbox_background": "#37394E",
  "textbox_border": "#303843",
  "checkbox_background": "#373B4E",
  "checkbox_background_hover": "#494E67",
  "checkbox_background_active": "#262636",
  "checkbox_border": "#4F5267",
  "checkbox_border_hover": "#565A77",
  "checkbox_border_active": "#2F3244",
  "checkbox_checked": "#C7C7D8",
  "checkbox_checked_bg": "#618EE6",
  "checkbox_checked_bg_hover": "#6080F9",
  "checkbox_checked_bg_active": "#495D9F",
  "checkbox_checked_border": "#7592E7",
  "checkbox_checked_border_hover": "#657DFF",
  "checkbox_checked_border_active": "#515D8F",
  "callout_background": "#2E280053",
  "callout_border": "#5B45008E",
  "callout_text": "#F0EBDE"
}
```
