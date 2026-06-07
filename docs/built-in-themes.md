# Built-in theme provenance

Bootty's built-in theme catalog uses the same restricted theme schema as user
themes: metadata plus `[colors]`. Theme files must not configure shell, input,
window, font, or app chrome settings.

See `docs/configuration.md` for theme lookup order and user theme file
locations.

Current built-ins:

| Theme | Source | License note |
| --- | --- | --- |
| Catppuccin Mocha | `catppuccin/ghostty` template and `mbadolato/iTerm2-Color-Schemes/ghostty/Catppuccin Mocha` | Catppuccin repository is MIT; iTerm2-Color-Schemes collection is MIT and notes individual theme authorship |
| Catppuccin Latte | `catppuccin/ghostty` template and `mbadolato/iTerm2-Color-Schemes/ghostty/Catppuccin Latte` | Catppuccin repository is MIT; iTerm2-Color-Schemes collection is MIT and notes individual theme authorship |
| TokyoNight Night | `mbadolato/iTerm2-Color-Schemes/ghostty/TokyoNight Night` | iTerm2-Color-Schemes collection is MIT and notes individual theme authorship |
| Gruvbox Dark | `mbadolato/iTerm2-Color-Schemes/ghostty/Gruvbox Dark` | iTerm2-Color-Schemes collection is MIT and notes individual theme authorship |

Source snapshots consulted during implementation:

- `catppuccin/ghostty`: `5a58926563ddacbde4a12b4a347464c2c6945393`
- `mbadolato/iTerm2-Color-Schemes`: `267128889e574c224b56084f06d648eb1970ce9c`

The resolver and schema are large-catalog-ready, but new built-ins should be
added only with source and license notes.
