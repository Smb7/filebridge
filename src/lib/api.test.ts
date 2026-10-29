import { describe, expect, it } from "vitest";
import {
  blankToNull,
  hostKeyPromptCopy,
  isAppError,
  isHostKeyStop,
  keyPathForMethod,
  planCredentialWrite,
  secretFromFields,
  sessionForConnection,
  type LiveSession,
} from "./api";

describe("isAppError", () => {
  it("accepts a code and a display message", () => {
    expect(
      isAppError({ code: "auth_failed", message: "Authentication failed" }),
    ).toBe(true);
  });

  it("rejects values that are not a safe error shape", () => {
    expect(isAppError(null)).toBe(false);
    expect(isAppError("auth_failed")).toBe(false);
    expect(isAppError({ code: "auth_failed" })).toBe(false);
    expect(isAppError({ message: "Authentication failed" })).toBe(false);
    expect(isAppError({ code: 1, message: "Authentication failed" })).toBe(
      false,
    );
  });
});

describe("connection form helpers", () => {
  it("sends a key path only for key file authentication", () => {
    expect(keyPathForMethod("key_file", " /home/ubuntu/.ssh/id_ed25519 ")).toBe(
      "/home/ubuntu/.ssh/id_ed25519",
    );
    expect(keyPathForMethod("password", "/home/ubuntu/.ssh/id_ed25519")).toBe(
      null,
    );
    expect(keyPathForMethod("key_file", "  ")).toBe(null);
  });

  it("turns blank optional fields into null", () => {
    expect(blankToNull("  ")).toBe(null);
    expect(blankToNull(" /etc/telegraf ")).toBe("/etc/telegraf");
  });

  it("does not send a secret unless the user typed one", () => {
    expect(secretFromFields("password", "  ", "", "")).toBe(null);
    expect(secretFromFields("agent", "hunter2", "key", "pass")).toBe(null);
    expect(secretFromFields("password", "hunter2", "ignored", "")).toEqual({
      password: "hunter2",
    });
    expect(secretFromFields("private_key", "", "ignored-key", "")).toEqual({
      privateKeyPem: "ignored-key",
    });
  });
});

describe("planCredentialWrite", () => {
  it("refuses secret changes while the vault is locked", () => {
    expect(
      planCredentialWrite({
        locked: true,
        methodChanged: false,
        authMethod: "password",
        hasSecret: true,
      }),
    ).toEqual({
      action: "refuse",
      message: "Unlock the vault before saving a password or key.",
    });
    expect(
      planCredentialWrite({
        locked: true,
        methodChanged: true,
        authMethod: "agent",
        hasSecret: false,
      }).action,
    ).toBe("refuse");
  });

  it("clears a secret when authentication no longer uses one", () => {
    expect(
      planCredentialWrite({
        locked: false,
        methodChanged: true,
        authMethod: "agent",
        hasSecret: false,
      }),
    ).toEqual({ action: "clear" });
    expect(
      planCredentialWrite({
        locked: false,
        methodChanged: false,
        authMethod: "password",
        hasSecret: false,
      }),
    ).toEqual({ action: "keep" });
    expect(
      planCredentialWrite({
        locked: false,
        methodChanged: false,
        authMethod: "password",
        hasSecret: true,
      }),
    ).toEqual({ action: "set" });
  });
});

describe("live sessions", () => {
  const live: LiveSession = {
    sessionId: "ses-1",
    connectionId: "conn-1",
    name: "prod-telegraf",
    username: "ubuntu",
    host: "10.0.0.8",
    port: 22,
  };

  it("finds the session for a saved connection and ignores the other pane", () => {
    expect(sessionForConnection([live], "conn-1")).toEqual(live);
    expect(sessionForConnection([live], "conn-2")).toBeUndefined();
    expect(sessionForConnection([], "conn-1")).toBeUndefined();
  });

  it("treats only an untrusted or changed host key as a connect stop", () => {
    expect(isHostKeyStop("host_key_unknown")).toBe(true);
    expect(isHostKeyStop("host_key_changed")).toBe(true);
    expect(isHostKeyStop("auth_failed")).toBe(false);
    expect(isHostKeyStop("vault_locked")).toBe(false);
  });
});

describe("host key prompt", () => {
  it("uses a louder replace action when the key changed", () => {
    expect(hostKeyPromptCopy("unknown").confirm).toBe("Trust this key");
    expect(hostKeyPromptCopy("changed").title).toBe("Host key changed");
    expect(hostKeyPromptCopy("changed").confirm).toBe("Replace trusted key");
    expect(hostKeyPromptCopy("changed").confirm).not.toBe(
      hostKeyPromptCopy("unknown").confirm,
    );
  });
});
