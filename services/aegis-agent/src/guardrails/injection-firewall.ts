/**
 * HYPERION Channel 06 — Deterministic Untrusted Metadata Sanitizer & Prompt-Injection Firewall.
 *
 * Treats all external strings (account aliases, memos, client order notes) as UNTRUSTED DATA.
 * Detects adversarial prompt-injection attempts, extracts spoofed hijack target account IDs,
 * and replaces tainted text with deterministic forensic quarantine markers before any LLM sees it.
 */

import { blake3 } from "@noble/hashes/blake3";
import { bytesToHex, utf8ToBytes } from "@noble/hashes/utils";
import type { QuarantinedInjectionFinding } from "../domain/types.js";

interface InjectionRule {
  readonly ruleId: string;
  readonly severity: "HIGH" | "CRITICAL";
  readonly pattern: RegExp;
}

const INJECTION_RULES: readonly InjectionRule[] = [
  {
    ruleId: "INJ-001-SYSTEM-OVERRIDE",
    severity: "CRITICAL",
    pattern:
      /\b(ignore\s+(all\s+)?(previous|prior|above)\s+(instructions|prompts|rules)|disregard\s+(all\s+)?prior|system\s+override|developer\s+mode|new\s+system\s+directive|you\s+are\s+now\s+an?\b|<\s*\/?\s*system\s*>|<\|im_start\|>|\[system\])/i
  },
  {
    ruleId: "INJ-002-TOOL-HIJACK",
    severity: "CRITICAL",
    pattern:
      /\b(execute_risk_mitigation|freeze_account|throttle_rate_limit|enable_volatility_guard|cancel_open_orders|query_ledger_merkle_audit|call\s+tool|invoke\s+mcp)\b/i
  },
  {
    ruleId: "INJ-003-POLICY-BYPASS",
    severity: "CRITICAL",
    pattern:
      /\b(bypass\s+policy|skip\s+human\s+approval|skip\s+approval|dry_run\s*[:=]\s*false|hitl1\.|rbk1\.|approved_by\s*[:=]|emergency\s+root|do\s+not\s+log)\b/i
  },
  {
    ruleId: "INJ-004-DESTRUCTIVE-EXFIL",
    severity: "HIGH",
    pattern:
      /\b(drain\s+treasury|transfer_funds|wipe\s+ledger|halt\s+all\s+markets|drop\s+table|freeze\s+(account|user|id|treasury|maker)s?\b)/i
  },
  {
    ruleId: "INJ-005-EMBEDDED-JSON-OR-BIDI",
    severity: "HIGH",
    pattern: /(\{"action"\s*:|"target_account_ids"\s*:|[\u202A-\u202E\u2066-\u2069])/i
  }
];

const ACCOUNT_ID_TOKEN_REGEX = /\b([1-9][0-9]{0,18})\b/g;

export interface SanitizedFieldResult {
  sanitizedText: string;
  tainted: boolean;
  findings: QuarantinedInjectionFinding[];
}

export class PromptInjectionFirewall {
  /**
   * Scans an untrusted string field associated with `ownerAccountId`.
   * If any injection pattern matches, extracts candidate hijack account IDs (excluding `ownerAccountId`)
   * and returns a redacted safe string so untrusted instructions never reach an LLM prompt.
   */
  public scanAndSanitizeField(params: {
    ownerAccountId: string;
    fieldName: QuarantinedInjectionFinding["field_name"];
    rawText: string;
  }): SanitizedFieldResult {
    const raw = params.rawText ?? "";
    if (raw.length === 0) {
      return { sanitizedText: "", tainted: false, findings: [] };
    }

    const rawDigest = bytesToHex(blake3(utf8ToBytes(raw)));
    const findings: QuarantinedInjectionFinding[] = [];

    for (const rule of INJECTION_RULES) {
      const match = rule.pattern.exec(raw);
      if (match) {
        const extractedIds = new Set<string>();
        ACCOUNT_ID_TOKEN_REGEX.lastIndex = 0;
        let idMatch = ACCOUNT_ID_TOKEN_REGEX.exec(raw);
        while (idMatch !== null) {
          const candidateId = idMatch[1];
          if (candidateId && candidateId !== params.ownerAccountId) {
            extractedIds.add(candidateId);
          }
          idMatch = ACCOUNT_ID_TOKEN_REGEX.exec(raw);
        }

        findings.push({
          account_id: params.ownerAccountId,
          field_name: params.fieldName,
          rule_id: rule.ruleId,
          severity: rule.severity,
          matched_excerpt: match[0].slice(0, 80),
          raw_blake3_digest: rawDigest,
          hijack_target_accounts_extracted: [...extractedIds]
        });
      }
    }

    if (findings.length === 0) {
      // Strip ASCII control characters even on clean strings without triggering control-regex warnings
      const safeClean = Array.from(raw)
        .map((ch) => {
          const code = ch.charCodeAt(0);
          return code < 0x20 || code === 0x7f ? " " : ch;
        })
        .join("")
        .trim()
        .slice(0, 160);
      return {
        sanitizedText: safeClean,
        tainted: false,
        findings: []
      };
    }

    const ruleList = findings.map((f) => f.rule_id).join(",");
    return {
      sanitizedText: `[QUARANTINED_UNTRUSTED_METADATA acct=${params.ownerAccountId} rules=${ruleList} blake3=${rawDigest.slice(0, 16)}]`,
      tainted: true,
      findings
    };
  }

  /**
   * Checks whether a tool `reason` string itself carries embedded prompt-injection or policy-bypass syntax.
   */
  public containsAdversarialDirective(text: string): {
    adversarial: boolean;
    matchedRuleIds: string[];
  } {
    const matchedRuleIds: string[] = [];
    for (const rule of INJECTION_RULES) {
      if (
        rule.ruleId === "INJ-001-SYSTEM-OVERRIDE" ||
        rule.ruleId === "INJ-003-POLICY-BYPASS" ||
        rule.ruleId === "INJ-005-EMBEDDED-JSON-OR-BIDI"
      ) {
        if (rule.pattern.test(text)) {
          matchedRuleIds.push(rule.ruleId);
        }
      }
    }
    return {
      adversarial: matchedRuleIds.length > 0,
      matchedRuleIds
    };
  }
}
