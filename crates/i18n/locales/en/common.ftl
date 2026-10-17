# Shared strings: the app name, buttons, relative time, language names,
# empty states. Keys start with `common-` (docs/design/i18n.md).

-app-name = Lectern

## Language picker. Each language is named in its own script, in both locales.

common-language-en = English
common-language-zh-cn = 简体中文
common-language-system = Match System

## Buttons

common-ok = OK
common-cancel = Cancel
common-close = Close
common-retry = Retry
common-restart = Restart { -app-name }

## Relative time. Strings::relative_time picks the message; $count is a whole number.

common-time-just-now = just now
common-time-minutes-ago = { $count } min ago
common-time-hours-ago = { $count ->
    [one] { $count } hour ago
   *[other] { $count } hours ago
}
common-time-days-ago = { $count ->
    [one] { $count } day ago
   *[other] { $count } days ago
}
common-time-weeks-ago = { $count ->
    [one] { $count } week ago
   *[other] { $count } weeks ago
}
common-time-months-ago = { $count ->
    [one] { $count } month ago
   *[other] { $count } months ago
}
common-time-years-ago = { $count ->
    [one] { $count } year ago
   *[other] { $count } years ago
}

## Empty states

common-empty = Nothing here yet
