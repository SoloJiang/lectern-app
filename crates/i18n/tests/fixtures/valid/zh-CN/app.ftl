-brand = Lectern
app-title = { -brand }
app-hello = 你好，{ $name }！
    .tooltip = 向 { $name } 问好
app-files = { $count ->
    [0] 没有文件
   *[other] { $count } 个文件
}
app-kind = { $kind ->
    [file] 文件
    [folder] 文件夹
   *[other] 项目
}
app-nested = { $kind ->
    [file] { $count } 个文件，在 { $where }
   *[other] { $count } 个项目，在 { $where }
}
app-ref = { app-title }：{ app-hello.tooltip }
app-literal = { "{" }{ 1.50 }{ "\u0041" }
app-only-attrs =
    .label = 标签
app-default-only = { $n } 个
