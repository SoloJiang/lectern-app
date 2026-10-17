// Deprecated: use example.com/acme/v2 instead.
module example.com/acme/api

go 1.22

require (
	github.com/stretchr/testify v1.9.0
	golang.org/x/sync v0.7.0 // indirect
)

retract [v1.0.0, v1.0.5]
