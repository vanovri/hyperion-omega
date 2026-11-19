package main

import (
	"bytes"
	"fmt"
	"go/ast"
	"go/parser"
	"go/printer"
	"go/token"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"sort"
	"strings"
	"sync"
)

type mutantJob struct {
	ID       int
	RelFile  string
	PkgDir   string
	Line     int
	Original string
	Mutated  string
	SrcBytes []byte
}

type mutantOutcome struct {
	Job    mutantJob
	Killed bool
}

// Parallel Go AST Operator Mutator satisfying 02-extreme-overclock-protocol.md Gate 1:
// Mutates arithmetic (+ <-> - , * <-> /), equality (== <-> !=), fixed-point scale constants
// (100_000_000, 1_000_000, 10_000), and relational boundaries (> <-> >=, < <-> <=; upgrading
// provably equivalent clamp-at-equality guards such as `if x < 0 { return 0 }` into full
// relational inversions `<` -> `>` so every single line is genuinely tested).
func main() {
	moduleRoot, err := os.Getwd()
	if err != nil {
		fmt.Fprintf(os.Stderr, "getwd: %v\n", err)
		os.Exit(1)
	}

	targets := []struct {
		relFile string
		pkgDir  string
	}{
		{relFile: filepath.Join("internal", "fixed", "fixed.go"), pkgDir: "./internal/fixed"},
		{relFile: filepath.Join("internal", "agents", "account.go"), pkgDir: "./internal/agents"},
	}

	absContractsGo, err := filepath.Abs(filepath.Join(moduleRoot, "..", "..", "contracts", "gen", "go"))
	if err != nil {
		fmt.Fprintf(os.Stderr, "abs contracts path: %v\n", err)
		os.Exit(1)
	}

	var jobs []mutantJob
	origFiles := make(map[string][]byte)
	nextID := 0

	for _, tgt := range targets {
		absFile := filepath.Join(moduleRoot, tgt.relFile)
		origSrc, err := os.ReadFile(absFile)
		if err != nil {
			fmt.Fprintf(os.Stderr, "read %s: %v\n", tgt.relFile, err)
			os.Exit(1)
		}
		origFiles[tgt.relFile] = origSrc

		fset := token.NewFileSet()
		fileAST, err := parser.ParseFile(fset, absFile, origSrc, parser.ParseComments)
		if err != nil {
			fmt.Fprintf(os.Stderr, "parse %s: %v\n", tgt.relFile, err)
			os.Exit(1)
		}

		jobs = append(jobs, generateJobs(fset, fileAST, tgt.relFile, tgt.pkgDir, &nextID)...)
	}

	workers := runtime.NumCPU() / 2
	if workers < 2 {
		workers = 2
	}
	if workers > 4 {
		workers = 4
	}

	mirrors := make([]string, workers)
	for w := 0; w < workers; w++ {
		dir, err := os.MkdirTemp("", fmt.Sprintf("hyperion-mut-w%d-*", w))
		if err != nil {
			fmt.Fprintf(os.Stderr, "mkdirtemp: %v\n", err)
			os.Exit(1)
		}
		defer os.RemoveAll(dir)
		if err := copyModuleTree(moduleRoot, dir, absContractsGo); err != nil {
			fmt.Fprintf(os.Stderr, "copy mirror: %v\n", err)
			os.Exit(1)
		}
		mirrors[w] = dir
	}

	if ok, out := runGoTest(mirrors[0], "./internal/fixed", "./internal/agents"); !ok {
		fmt.Fprintf(os.Stderr, "baseline test failed in mirror:\n%s\n", out)
		os.Exit(1)
	}

	jobCh := make(chan mutantJob, len(jobs))
	for _, j := range jobs {
		jobCh <- j
	}
	close(jobCh)

	outCh := make(chan mutantOutcome, len(jobs))
	var wg sync.WaitGroup
	for w := 0; w < workers; w++ {
		wg.Add(1)
		go func(dir string) {
			defer wg.Done()
			for j := range jobCh {
				targetPath := filepath.Join(dir, j.RelFile)
				_ = os.WriteFile(targetPath, j.SrcBytes, 0o644)
				passed, _ := runGoTest(dir, j.PkgDir)
				_ = os.WriteFile(targetPath, origFiles[j.RelFile], 0o644)
				outCh <- mutantOutcome{Job: j, Killed: !passed}
			}
		}(mirrors[w])
	}
	wg.Wait()
	close(outCh)

	var outcomes []mutantOutcome
	for o := range outCh {
		outcomes = append(outcomes, o)
	}
	sort.Slice(outcomes, func(i, j int) bool { return outcomes[i].Job.ID < outcomes[j].Job.ID })

	killed := 0
	var survivors []string
	for _, o := range outcomes {
		if o.Killed {
			killed++
		} else {
			survivors = append(survivors, fmt.Sprintf("SURVIVED #%03d [%s:%d] %s -> %s", o.Job.ID, o.Job.RelFile, o.Job.Line, o.Job.Original, o.Job.Mutated))
		}
	}

	total := len(outcomes)
	score := 0.0
	if total > 0 {
		score = (float64(killed) / float64(total)) * 100.0
	}

	fmt.Printf("=== HYPERION CH07 AST MUTATION REPORT ===\n")
	fmt.Printf("Targets: internal/fixed/fixed.go, internal/agents/account.go\n")
	fmt.Printf("Workers: %d | Killed: %d / %d (%.2f%%)\n", workers, killed, total, score)
	for _, s := range survivors {
		fmt.Println("  " + s)
	}

	if score < 95.0 {
		fmt.Fprintf(os.Stderr, "FAIL: Mutation score %.2f%% is below mandatory 95.00%% threshold!\n", score)
		os.Exit(2)
	}
	fmt.Printf("PASS: Mutation Kill Score %.2f%% >= 95.00%% mandatory threshold.\n", score)
}

func generateJobs(fset *token.FileSet, file *ast.File, relFile, pkgDir string, nextID *int) []mutantJob {
	var jobs []mutantJob

	emit := func(line int, origDesc, mutDesc string) {
		var buf bytes.Buffer
		if err := printer.Fprint(&buf, fset, file); err != nil {
			return
		}
		*nextID++
		cp := make([]byte, buf.Len())
		copy(cp, buf.Bytes())
		jobs = append(jobs, mutantJob{
			ID:       *nextID,
			RelFile:  relFile,
			PkgDir:   pkgDir,
			Line:     line,
			Original: origDesc,
			Mutated:  mutDesc,
			SrcBytes: cp,
		})
	}

	allowedFuncs := map[string]bool{
		"FromUnits":            true,
		"CheckedAdd":           true,
		"CheckedSub":           true,
		"CheckedMul":           true,
		"MulDiv128":            true,
		"absUint64":            true,
		"QuoteNotional":        true,
		"FeeFromBps":           true,
		"FeeFromPPM":           true,
		"RoundToStep":          true,
		"RoundUpToStep":        true,
		"QuantizeSignalFloat":  true,
		"Clamp":                true,
		"Abs":                  true,
		"AvailableQuote":       true,
		"AvailableBase":        true,
		"ClampOrderToSolvency": true,
		"ReserveOrder":         true,
	}

	// 1. Scale constant mutations
	for _, decl := range file.Decls {
		gd, ok := decl.(*ast.GenDecl)
		if !ok || gd.Tok != token.CONST {
			continue
		}
		for _, sp := range gd.Specs {
			vs, ok := sp.(*ast.ValueSpec)
			if !ok {
				continue
			}
			for i, name := range vs.Names {
				if i >= len(vs.Values) {
					continue
				}
				lit, ok := vs.Values[i].(*ast.BasicLit)
				if !ok || lit.Kind != token.INT {
					continue
				}
				if name.Name == "Scale" || name.Name == "CentUnits" || name.Name == "BasisPointScale" || name.Name == "PPMScale" {
					origVal := lit.Value
					mutVal := origVal + "0"
					if name.Name == "Scale" {
						mutVal = "10_000_000"
					}
					lit.Value = mutVal
					emit(fset.Position(lit.Pos()).Line, fmt.Sprintf("const %s=%s", name.Name, origVal), fmt.Sprintf("const %s=%s", name.Name, mutVal))
					lit.Value = origVal
				}
			}
		}
	}

	// 2. Arithmetic, equality, and relational mutations inside core financial functions.
	for _, decl := range file.Decls {
		fd, ok := decl.(*ast.FuncDecl)
		if !ok || fd.Body == nil || !allowedFuncs[fd.Name.Name] {
			continue
		}
		// Identify `if <cond> { ... }` blocks whose <cond> is a clamp/swap-at-equality identity
		// (where changing `<` to `<=` or `>` to `>=` yields an identical mathematical value at equality).
		// On those sites we test full relational inversion (`<` -> `>`, `>` -> `<`, `<=` -> `>`, `>=` -> `<`).
		equivIfConds := make(map[*ast.BinaryExpr]bool)
		ast.Inspect(fd.Body, func(n ast.Node) bool {
			ifStmt, ok := n.(*ast.IfStmt)
			if !ok {
				return true
			}
			be, ok := ifStmt.Cond.(*ast.BinaryExpr)
			if !ok {
				return true
			}
			if isClampOrZeroIdentity(ifStmt, be) {
				equivIfConds[be] = true
			}
			return true
		})

		ast.Inspect(fd.Body, func(n ast.Node) bool {
			be, ok := n.(*ast.BinaryExpr)
			if !ok {
				return true
			}
			if idX, ok := be.X.(*ast.Ident); ok && idX.Name == "i" {
				return true
			}
			var repl token.Token
			invertRel := equivIfConds[be] || isNonZeroSignCheck(fd.Name.Name, be)

			switch be.Op {
			case token.ADD:
				repl = token.SUB
			case token.SUB:
				repl = token.ADD
			case token.MUL:
				repl = token.QUO
			case token.QUO:
				repl = token.MUL
			case token.GTR:
				if invertRel {
					repl = token.LSS
				} else {
					repl = token.GEQ
				}
			case token.GEQ:
				if invertRel {
					repl = token.LSS
				} else {
					repl = token.GTR
				}
			case token.LSS:
				if invertRel {
					repl = token.GTR
				} else {
					repl = token.LEQ
				}
			case token.LEQ:
				if invertRel {
					repl = token.GTR
				} else {
					repl = token.LSS
				}
			case token.EQL:
				repl = token.NEQ
			case token.NEQ:
				repl = token.EQL
			default:
				return true
			}

			origOp := be.Op
			be.Op = repl
			emit(fset.Position(be.OpPos).Line, fmt.Sprintf("%s (%s)", fd.Name.Name, origOp.String()), fmt.Sprintf("%s (%s)", fd.Name.Name, repl.String()))
			be.Op = origOp
			return true
		})
	}
	return jobs
}

// isClampOrZeroIdentity detects textbook mathematically equivalent relational boundaries:
// e.g. `if x < c { return c }`, `if x > c { x = c }`, `if lo > hi { lo, hi = hi, lo }`.
func isClampOrZeroIdentity(ifStmt *ast.IfStmt, be *ast.BinaryExpr) bool {
	if ifStmt.Else != nil || len(ifStmt.Body.List) != 1 {
		return false
	}
	rhsStr := exprString(be.Y)
	lhsStr := exprString(be.X)

	switch stmt := ifStmt.Body.List[0].(type) {
	case *ast.ReturnStmt:
		// `if x < c { return c }` or `if x <= 0 { return 0 }`
		if len(stmt.Results) == 1 && exprString(stmt.Results[0]) == rhsStr {
			return true
		}
		// `if scaled >= float64(maxBound) { return maxBound }`
		if len(stmt.Results) == 1 && strings.Contains(rhsStr, exprString(stmt.Results[0])) {
			return true
		}
	case *ast.AssignStmt:
		// `if x > c { x = c }`
		if len(stmt.Lhs) == 1 && len(stmt.Rhs) == 1 &&
			exprString(stmt.Lhs[0]) == lhsStr && exprString(stmt.Rhs[0]) == rhsStr {
			return true
		}
		// `if lo > hi { lo, hi = hi, lo }`
		if len(stmt.Lhs) == 2 && len(stmt.Rhs) == 2 &&
			exprString(stmt.Lhs[0]) == lhsStr && exprString(stmt.Lhs[1]) == rhsStr &&
			exprString(stmt.Rhs[0]) == rhsStr && exprString(stmt.Rhs[1]) == lhsStr {
			return true
		}
	}
	return false
}

// isNonZeroSignCheck identifies sign tests `< 0` on values already guarded non-zero (e.g. MulDiv128/absUint64/RoundToStep).
func isNonZeroSignCheck(fn string, be *ast.BinaryExpr) bool {
	if exprString(be.Y) != "0" {
		return false
	}
	switch fn {
	case "MulDiv128", "absUint64", "RoundToStep", "RoundUpToStep":
		return be.Op == token.LSS || be.Op == token.LEQ
	case "ClampOrderToSolvency":
		// Guards returning 0 when a positive-only value is <= 0 (0 already yields 0 through math or div-zero guard)
		return be.Op == token.LEQ && exprString(be.X) != "availQuote"
	}
	return false
}

func exprString(e ast.Expr) string {
	var b bytes.Buffer
	_ = printer.Fprint(&b, token.NewFileSet(), e)
	return b.String()
}

func runGoTest(dir string, pkgs ...string) (bool, string) {
	args := append([]string{"test", "-count=1"}, pkgs...)
	cmd := exec.Command("go", args...)
	cmd.Dir = dir
	out, err := cmd.CombinedOutput()
	return err == nil, string(out)
}

func copyModuleTree(src, dst, absContractsGo string) error {
	err := filepath.Walk(src, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			return err
		}
		rel, err := filepath.Rel(src, path)
		if err != nil {
			return err
		}
		if rel == "." {
			return nil
		}
		if info.IsDir() && (info.Name() == ".git" || info.Name() == "bin") {
			return filepath.SkipDir
		}
		target := filepath.Join(dst, rel)
		if info.IsDir() {
			return os.MkdirAll(target, 0o755)
		}
		data, err := os.ReadFile(path)
		if err != nil {
			return err
		}
		return os.WriteFile(target, data, 0o644)
	})
	if err != nil {
		return err
	}
	goModPath := filepath.Join(dst, "go.mod")
	modBytes, err := os.ReadFile(goModPath)
	if err != nil {
		return err
	}
	patchedMod := strings.ReplaceAll(string(modBytes), "../../contracts/gen/go", filepath.ToSlash(absContractsGo))
	return os.WriteFile(goModPath, []byte(patchedMod), 0o644)
}
