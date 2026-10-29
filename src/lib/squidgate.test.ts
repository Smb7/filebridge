import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { parse } from "yaml";

const workflowText = readFileSync(".github/workflows/squidgate.yml", "utf8");
const policyText = readFileSync(".github/squidgate.yml", "utf8");

const pinnedBuild = "bfc3a29742a3d6a121f9e6b9662260d704d71b8e";

type Workflow = {
  on: {
    pull_request: {
      types: string[];
    };
  };
  permissions: Record<string, string>;
  jobs: {
    squidgate: {
      name: string;
      permissions: Record<string, string>;
      steps: Array<{
        uses?: string;
        with?: Record<string, string>;
      }>;
    };
  };
};

type Policy = {
  llm: {
    provider: string;
    model: string;
  };
  policy: {
    block_on: string;
    categories: Record<string, boolean>;
  };
  output: {
    fail_on_error: boolean;
  };
};

const workflow = parse(workflowText) as Workflow;
const policy = parse(policyText) as Policy;

describe("SquidGate workflow", () => {
  it("scans only the three pull request events", () => {
    expect(Object.keys(workflow.on)).toEqual(["pull_request"]);
    expect(workflow.on.pull_request.types).toEqual([
      "opened",
      "synchronize",
      "reopened",
    ]);
    expect(workflowText).not.toContain("pull_request_target");
  });

  it("keeps write permissions on the squidgate job", () => {
    expect(workflow.permissions).toEqual({ contents: "read" });
    expect(workflow.jobs.squidgate.name).toBe("squidgate");
    expect(workflow.jobs.squidgate.permissions).toEqual({
      contents: "read",
      "pull-requests": "write",
      checks: "write",
    });
  });

  it("pins the immutable build and does not set a base URL", () => {
    const step = workflow.jobs.squidgate.steps.find((item) =>
      item.uses?.startsWith("SquidSec/SquidGate@"),
    );
    expect(step?.uses).toBe(`SquidSec/SquidGate@${pinnedBuild}`);
    expect(step?.with).toEqual({
      "llm-api-key": "${{ secrets.LLM_API_KEY }}",
      "llm-provider": "opencode-go",
      "llm-model": "glm-5.3-flash",
    });
    expect(workflowText).not.toContain("llm-base-url");
    expect(workflowText).not.toMatch(/sk-[A-Za-z0-9]/);
  });
});

describe("SquidGate policy", () => {
  it("blocks high findings and keeps secrets and injection on", () => {
    expect(policy.llm).toEqual({
      provider: "opencode-go",
      model: "glm-5.3-flash",
    });
    expect(policy.policy.block_on).toBe("high");
    expect(policy.policy.categories.secrets).toBe(true);
    expect(policy.policy.categories.injection).toBe(true);
    expect(policy.output.fail_on_error).toBe(true);
    expect(policyText).not.toContain("llm-base-url");
    expect(policyText).not.toMatch(/sk-[A-Za-z0-9]/);
    expect(policyText).not.toContain("LLM_API_KEY");
  });
});
