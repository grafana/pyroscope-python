.PHONY: ci-matrix
ci-matrix:
	@go run ./cmd/ci-matrix

.PHONY: test/%
test/%:
	go test -v -timeout 10m -count=1 -run '^$*$$' .
