import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const workflow = readFileSync(".github/workflows/squidgate.yml", "utf8");
const policy = readFileSync(".github/squidgate.yml", "utf8");

const pinnedBuild = "bfc3a29742a3d6a121f9e6b9662260d704d71b8e";

describe("SquidGate workflow", () => {
  it("scans pull requests only and pins the immutable build", () => {
    expect(workflow).not.toContain("pull_request_target");
    expect(workflow).toMatch(/^ {2}pull_request:$/m);
    expect(workflow).toContain(`SquidSec/SquidGate@${pinnedBuild}`);
    expect(workflow).not.toMatch(/SquidSec\/SquidGate@v\d/);
    expect(workflow).toContain("llm-provider: opencode-go");
    expect(workflow).toContain("llm-model: glm-5.3-flash");
    expect(workflow).toContain("llm-api-key: ${{ secrets.LLM_API_KEY }}");
    expect(workflow).not.toContain("llm-base-url");
    expect(workflow).toContain("contents: read");
    expect(workflow).toContain("pull-requests: write");
    expect(workflow).toContain("checks: write");
  });

  it("does not embed a provider key", () => {
    expect(workflow).not.toMatch(/sk-[A-Za-z0-9]/);
    expect(policy).not.toMatch(/sk-[A-Za-z0-9]/);
    expect(policy).not.toContain("LLM_API_KEY");
  });
});

describe("SquidGate policy", () => {
  it("blocks high findings and keeps secrets and injection on", () => {
    expect(policy).toContain("provider: opencode-go");
    expect(policy).toContain("model: glm-5.3-flash");
    expect(policy).toContain("block_on: high");
    expect(policy).toMatch(/secrets:\s*true/);
    expect(policy).toMatch(/injection:\s*true/);
    expect(policy).toContain("fail_on_error: true");
    expect(policy).not.toContain("llm-base-url");
  });
});
