app-function = { NUMBER($count) }
app-term-args = { -brand(case: "upper") }
app-selector = { "x" ->
   *[other] x
}
app-term-var = { -greeting }
app-decimal-key = { $n ->
    [1.5] one and a half
   *[other] other
}
app-mixed-keys = { $n ->
    [0] zero
    [file] file
   *[other] other
}
-brand = Lectern
    .gender = neuter
-greeting = Hello { $name }
