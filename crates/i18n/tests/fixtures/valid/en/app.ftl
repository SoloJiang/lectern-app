-brand = Lectern
app-title = { -brand }
app-hello = Hello, { $name }!
    .tooltip = Say hello to { $name }
app-files = { $count ->
    [0] No files
    [one] One file
   *[other] { $count } files
}
app-kind = { $kind ->
    [file] File
    [folder] Folder
   *[other] Item
}
app-nested = { $count ->
    [one] { $kind ->
        [file] One file in { $where }
       *[other] One item in { $where }
    }
   *[other] { $count } things in { $where }
}
app-ref = { app-title }: { app-hello.tooltip }
app-literal = { "{" }{ 1.50 }{ "\u0041" }
app-only-attrs =
    .label = Label
app-default-only = { $n ->
   *[other] { $n } of something
}
