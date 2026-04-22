# Layout Customization

Hummingbird can load a custom UI layout from a JSON file in its data directory

## Where layout files live

Custom layout files go in the `ui/` folder inside Hummingbird's data directory:

| Platform | Folder                                                      |
| -------- | ----------------------------------------------------------- |
| Linux    | `~/.local/share/hummingbird/ui/`                            |
| macOS    | `~/Library/Application Support/org.mailliw.hummingbird/ui/` |
| Windows  | `%appdata%\\mailliw\\hummingbird\\data\\ui\\`               |

On first run, Hummingbird creates a starter file at `ui/custom.json`. (might change)

## What a layout file can change

- `layout`: the app layout
- `font`: the main UI font family

## Example

```json
{
    "layout": {
        "outer_order": ["main", "controls"],
        "main_order": ["library_sidebar", "library_content", "side_panel"],
        "library": {
            "two_column_order": ["browse", "detail"]
        }
    },
    "font": "Inter"
}
```

(picture goes here)

## How to use one

1. Create or edit a JSON file in the `ui/` folder
2. Open **Settings > Interface > Layout**
3. Select that file

If no file is selected, Hummingbird uses its built-in default layout.
Selecting a different file takes effect immediately. Editing the selected file still requires restarting Hummingbird.

## Supported layout values

### `layout.outer_order`

Controls whether the main content or the controls come first below the header

Allowed values:

- `["main", "controls"]`
- `["controls", "main"]`

### `layout.main_order`

Controls the order of the three main regions inside the main band

The allowed region names are:

- `library_sidebar`
- `library_content`
- `side_panel`

### `layout.library.two_column_order`

Controls the order of the two-column library split when two-column mode is enabled

Allowed values:

- `["browse", "detail"]`
- `["detail", "browse"]`

## Validation and fallback

If the selected file is missing or contains invalid JSON, Hummingbird falls back to the built-in default UI layout

If the JSON is valid but the layout contains unsupported or duplicate values, Hummingbird also falls back to the built-in default UI layout
