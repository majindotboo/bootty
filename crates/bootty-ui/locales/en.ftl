## Stable Bootty presentation keys. Insert user data as variables, never into message source.
menu-settings = Settings…
menu-about = About Bootty
menu-quit = Quit Bootty
settings-search = Search settings
settings-navigation = Settings Navigation
common-save = Save
command-finished = Command finished
command-finished-status = Command finished with exit status { $status }
command-duration = { $seconds ->
    [one] Completed in { $seconds } second
   *[other] Completed in { $seconds } seconds
    }
document-unsaved = Save changes to { $name }?
settings-search-placeholder = Search settings…

## Settings, sections, pages and commands keep their English with their schema and catalog.
## Translations add keys derived from stable paths (see `presentation_key`), never from labels.
find-title = Find
find-previous = Previous
find-next = Next
find-regex = Regular expression
find-case = Case sensitive
palette-title = Commands
palette-search = Search commands…
palette-empty = No matching commands
palette-hint = ↑ ↓ navigate   Enter run   Esc close
palette-count = { $total ->
    [one] { $shown } / { $total } command
   *[other] { $shown } / { $total } commands
    }

