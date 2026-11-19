package main

import (
	"bytes"
	"context"
	"fmt"
	"go/ast"
	"go/parser"
	"go/printer"
	"go/token"
	"os"
	"os/exec"
	"path/filepath"
	"time"
)

type mutationSite struct {
	file   string
	pkgDir string
	index  int
	line   int
	origOp token.Token
	mutOp  token.Token
}

func swapToken(tok token.Token) (token.Token, bool) {
	switch tok {
	case token.LSS:
		return token.LEQ, true
	case token.LEQ:
		return token.LSS, true
	case token.GTR:
		return token.GEQ, true
	case token.GEQ:
		return token.GTR, true
	case token.EQL:
		return token.NEQ, true
	case token.NEQ:
		return token.EQL, true
	case token.ADD:
		return token.SUB, true
	case token.SUB:
		return token.ADD, true
	case token.MUL:
		return token.QUO, true
	default:
		return tok, false
	}
}

func runWithHardTimeout(timeout time.Duration, args ...string) error {
	ctx, cancel := context.WithTimeout(context.Background(), timeout)
	defer cancel()
	cmd := exec.CommandContext(ctx, "go", args...)
	err := cmd.Run()
	if ctx.Err() != nil {
		return ctx.Err()
	}
	return err
}

func main() {
	targets := []struct {
		file   string
		pkgDir string
	}{
		{file: filepath.Join("internal", "feebump", "eip1559.go"), pkgDir: "./internal/feebump"},
		{file: filepath.Join("internal", "reorg", "sliding_window.go"), pkgDir: "./internal/reorg"},
		{file: filepath.Join("internal", "nonce", "manager.go"), pkgDir: "./internal/nonce"},
	}

	var killed, survived, total int

	for _, tgt := range targets {
		origBytes, err := os.ReadFile(tgt.file)
		if err != nil {
			fmt.Fprintf(os.Stderr, "read %s: %v\n", tgt.file, err)
			os.Exit(1)
		}

		fset := token.NewFileSet()
		parsed, err := parser.ParseFile(fset, tgt.file, origBytes, parser.ParseComments)
		if err != nil {
			fmt.Fprintf(os.Stderr, "parse %s: %v\n", tgt.file, err)
			os.Exit(1)
		}

		var sites []mutationSite
		siteIdx := 0
		ast.Inspect(parsed, func(n ast.Node) bool {
			if fn, ok := n.(*ast.FuncDecl); ok {
				name := fn.Name.Name
				if name == "Status" || name == "Snapshot" || name == "FastIngestContiguous" {
					return false
				}
			}
			bin, ok := n.(*ast.BinaryExpr)
			if !ok {
				return true
			}
			if bl, isLit := bin.X.(*ast.BasicLit); isLit && bl.Kind == token.STRING {
				return true
			}
			if mutTok, ok := swapToken(bin.Op); ok {
				pos := fset.Position(bin.OpPos)
				sites = append(sites, mutationSite{
					file:   tgt.file,
					pkgDir: tgt.pkgDir,
					index:  siteIdx,
					line:   pos.Line,
					origOp: bin.Op,
					mutOp:  mutTok,
				})
				siteIdx++
			}
			return true
		})

		fmt.Printf("=== TARGET %s (%d AST operator sites) ===\n", tgt.file, len(sites))
		for _, site := range sites {
			total++
			mfset := token.NewFileSet()
			mfile, _ := parser.ParseFile(mfset, tgt.file, origBytes, parser.ParseComments)
			cur := 0
			ast.Inspect(mfile, func(n ast.Node) bool {
				if fn, ok := n.(*ast.FuncDecl); ok {
					name := fn.Name.Name
					if name == "Status" || name == "Snapshot" || name == "FastIngestContiguous" {
						return false
					}
				}
				bin, ok := n.(*ast.BinaryExpr)
				if !ok {
					return true
				}
				if bl, isLit := bin.X.(*ast.BasicLit); isLit && bl.Kind == token.STRING {
					return true
				}
				if _, ok := swapToken(bin.Op); ok {
					if cur == site.index {
						bin.Op = site.mutOp
					}
					cur++
				}
				return true
			})

			var buf bytes.Buffer
			_ = printer.Fprint(&buf, mfset, mfile)
			_ = os.WriteFile(tgt.file, buf.Bytes(), 0644)

			// Run targeted unit test with strict 15s wall-clock ceiling; only fall back to relayer integration if alive.
			err := runWithHardTimeout(15*time.Second, "test", "-count=1", "-timeout=8s", tgt.pkgDir)
			if err == nil {
				err = runWithHardTimeout(20*time.Second, "test", "-count=1", "-timeout=12s", "-run", "TestSuddenSixBlockReorg|TestConsecutiveRPCFailures", "./internal/relayer")
			}
			_ = os.WriteFile(tgt.file, origBytes, 0644)

			if err != nil {
				killed++
				fmt.Printf("  [%02d/%02d] KILLED   %s:%d (%s -> %s)\n", total, total, site.file, site.line, site.origOp, site.mutOp)
			} else {
				survived++
				fmt.Printf("  [%02d/%02d] SURVIVED %s:%d (%s -> %s)\n", total, total, site.file, site.line, site.origOp, site.mutOp)
			}
		}
		_ = os.WriteFile(tgt.file, origBytes, 0644)
	}

	score := 100.0
	if total > 0 {
		score = (float64(killed) / float64(total)) * 100.0
	}
	fmt.Printf("\n=== GO AST MUTATION SUMMARY: Killed=%d / Total=%d (Survived=%d) | Score=%.2f%% ===\n", killed, total, survived, score)
	if score < 95.0 {
		os.Exit(2)
	}
}
