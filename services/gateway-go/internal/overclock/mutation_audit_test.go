package overclock_test

import (
	"bytes"
	"context"
	"fmt"
	"go/ast"
	"go/parser"
	"go/printer"
	"go/token"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"
)

type targetSpec struct {
	relFile string
	pkgPath string
}

type mutantSite struct {
	relFile string
	pkgPath string
	line    int
	origOp  token.Token
	mutOp   token.Token
	nodeIdx int
}

// mutateOp maps core comparison & arithmetic operators to adversarial logical inversions
// per .agents/rules/02-extreme-overclock-protocol.md §1.
func mutateOp(op token.Token) (token.Token, bool) {
	switch op {
	case token.EQL: // == -> !=
		return token.NEQ, true
	case token.NEQ: // != -> ==
		return token.EQL, true
	case token.GTR: // > -> <=
		return token.LEQ, true
	case token.GEQ: // >= -> <
		return token.LSS, true
	case token.LSS: // < -> >=
		return token.GEQ, true
	case token.LEQ: // <= -> >
		return token.GTR, true
	case token.REM: // % -> / (tick/lot alignment modulo bypass)
		return token.QUO, true
	default:
		return op, false
	}
}

// TestMutationSurvivalAudit executes real Go AST operator mutation testing in parallel across the
// 5 core security, fixed-point alignment, rate-limiting, circuit-breaking, and binary wire-framing modules.
// Skipped unless HYPERION_RUN_MUTATION=1 so standard `go test -race ./...` remains fast.
func TestMutationSurvivalAudit(t *testing.T) {
	if os.Getenv("HYPERION_RUN_MUTATION") != "1" {
		t.Skip("skipping full AST compiler mutation sweep unless HYPERION_RUN_MUTATION=1")
	}

	serviceRoot, err := filepath.Abs(filepath.Join("..", ".."))
	if err != nil {
		t.Fatal(err)
	}
	monorepoRoot := filepath.Dir(filepath.Dir(serviceRoot))

	targets := []targetSpec{
		{relFile: filepath.Join("internal", "instruments", "registry.go"), pkgPath: "./internal/instruments"},
		{relFile: filepath.Join("internal", "auth", "signature.go"), pkgPath: "./internal/auth"},
		{relFile: filepath.Join("internal", "ws", "protocol.go"), pkgPath: "./internal/ws"},
		{relFile: filepath.Join("internal", "grpcclient", "breaker.go"), pkgPath: "./internal/grpcclient"},
		{relFile: filepath.Join("internal", "ratelimit", "limiter.go"), pkgPath: "./internal/ratelimit"},
	}

	type pkgResult struct {
		total     int
		killed    int
		survivors []string
		err       error
	}

	results := make([]pkgResult, len(targets))
	var wg sync.WaitGroup

	for ti, tgt := range targets {
		wg.Add(1)
		go func(idx int, spec targetSpec) {
			defer wg.Done()
			sandboxRoot := t.TempDir()
			if cerr := copyDir(filepath.Join(monorepoRoot, "contracts", "gen", "go"), filepath.Join(sandboxRoot, "contracts", "gen", "go")); cerr != nil {
				results[idx].err = cerr
				return
			}
			sandboxSvc := filepath.Join(sandboxRoot, "services", "gateway-go")
			if cerr := copyDir(serviceRoot, sandboxSvc); cerr != nil {
				results[idx].err = cerr
				return
			}

			fullPath := filepath.Join(sandboxSvc, spec.relFile)
			origBytes, rerr := os.ReadFile(fullPath)
			if rerr != nil {
				results[idx].err = rerr
				return
			}

			fset := token.NewFileSet()
			fileAST, perr := parser.ParseFile(fset, fullPath, origBytes, 0)
			if perr != nil {
				results[idx].err = perr
				return
			}

			var sites []mutantSite
			nodeCounter := 0
			ast.Inspect(fileAST, func(n ast.Node) bool {
				bin, ok := n.(*ast.BinaryExpr)
				if !ok {
					return true
				}
				cur := nodeCounter
				nodeCounter++

				mutOp, ok := mutateOp(bin.Op)
				if !ok {
					return true
				}
				if isIdentName(bin.Y, "nil") || isIdentName(bin.X, "err") || isIdentName(bin.X, "aerr") || isIdentName(bin.X, "rerr") {
					return true
				}
				if len(sites) < 8 {
					pos := fset.Position(bin.OpPos)
					sites = append(sites, mutantSite{
						relFile: spec.relFile,
						pkgPath: spec.pkgPath,
						line:    pos.Line,
						origOp:  bin.Op,
						mutOp:   mutOp,
						nodeIdx: cur,
					})
				}
				return true
			})

			res := pkgResult{total: len(sites)}
			for _, m := range sites {
				mfset := token.NewFileSet()
				mAST, merr := parser.ParseFile(mfset, fullPath, origBytes, 0)
				if merr != nil {
					res.err = merr
					break
				}
				cur := 0
				ast.Inspect(mAST, func(n ast.Node) bool {
					bin, ok := n.(*ast.BinaryExpr)
					if !ok {
						return true
					}
					if cur == m.nodeIdx {
						bin.Op = m.mutOp
					}
					cur++
					return true
				})
				var buf bytes.Buffer
				if err := printer.Fprint(&buf, mfset, mAST); err != nil {
					res.err = err
					break
				}
				if err := os.WriteFile(fullPath, buf.Bytes(), 0o644); err != nil {
					res.err = err
					break
				}

				ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
				cmd := exec.CommandContext(ctx, "go", "test", "-count=1", "-timeout=12s", m.pkgPath)
				cmd.Dir = sandboxSvc
				cmd.Env = append(os.Environ(), "CGO_ENABLED=0")
				runErr := cmd.Run()
				cancel()

				_ = os.WriteFile(fullPath, origBytes, 0o644)
				if runErr != nil {
					res.killed++
				} else {
					res.survivors = append(res.survivors, fmt.Sprintf("%s:%d (%s -> %s)", filepath.ToSlash(m.relFile), m.line, m.origOp, m.mutOp))
				}
			}
			results[idx] = res
		}(ti, tgt)
	}
	wg.Wait()

	total := 0
	killed := 0
	var allSurvivors []string
	for i, r := range results {
		if r.err != nil {
			t.Fatalf("mutation worker %s failed: %v", targets[i].pkgPath, r.err)
		}
		total += r.total
		killed += r.killed
		allSurvivors = append(allSurvivors, r.survivors...)
		t.Logf("Module %-24s : Killed %d / %d", targets[i].pkgPath, r.killed, r.total)
	}

	if total == 0 {
		t.Fatal("zero AST mutants discovered")
	}
	scoreBP := (killed * 10000) / total
	t.Logf("MUTATION AUDIT SUMMARY: Killed %d / %d (%d.%02d%%) | Survivors: %v",
		killed, total, scoreBP/100, scoreBP%100, allSurvivors)

	if scoreBP < 9500 {
		t.Fatalf("MUTATION GATE FAILED: score %d.%02d%% < 95.00%% required! Survivors: %s",
			scoreBP/100, scoreBP%100, strings.Join(allSurvivors, "; "))
	}
}

func isIdentName(expr ast.Expr, name string) bool {
	id, ok := expr.(*ast.Ident)
	return ok && id.Name == name
}

func copyDir(src, dst string) error {
	return filepath.Walk(src, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			return err
		}
		rel, err := filepath.Rel(src, path)
		if err != nil {
			return err
		}
		target := filepath.Join(dst, rel)
		if info.IsDir() {
			return os.MkdirAll(target, 0o755)
		}
		in, err := os.Open(path)
		if err != nil {
			return err
		}
		defer in.Close()
		out, err := os.OpenFile(target, os.O_CREATE|os.O_WRONLY|os.O_TRUNC, info.Mode())
		if err != nil {
			return err
		}
		_, cerr := io.Copy(out, in)
		_ = out.Close()
		return cerr
	})
}
