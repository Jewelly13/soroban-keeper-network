/**
 * Test suite for verifier-aware proof generation
 *
 * generateProof() must:
 *   - Fall back to the base MVP placeholder proof when a task has no
 *     verifier attached, or no SIGNATURE_PROOF_SECRET_KEY is configured.
 *   - Produce a signature-verifier-compatible proof (a raw 64-byte ed25519
 *     signature, hex-encoded) when both are present.
 *
 * buildSignatureVerifierMessage() must produce byte-identical output to
 * the contract-side signature_verifier::signed_message — pinned against a
 * reference value cross-checked against
 * contracts/verifiers/signature-verifier/src/test.rs's
 * test_signed_message_matches_keeper_bot_js_encoding, which computes the
 * same message from the Rust side for the same inputs.
 */

"use strict";

const { describe, it } = require("node:test");
const assert = require("node:assert");
const { Keypair } = require("@stellar/stellar-sdk");

const {
  buildSignatureVerifierMessage,
  signProofForTask,
  generateProof,
} = require("../index.js");

describe("buildSignatureVerifierMessage", () => {
  it("matches the reference byte layout computed on the Rust side", () => {
    const task = {
      owner: "GBWD63M5YQ3MV6VPYO74NFO3FJBGYXPCL25HSHXHOXKE7TBP2H5IICTP",
      calldata: Buffer.from("hello"),
      deadline: 123456n,
      reward: 1000000n,
    };

    const message = buildSignatureVerifierMessage(task);

    // See contracts/verifiers/signature-verifier/src/test.rs's
    // test_signed_message_matches_keeper_bot_js_encoding for the same
    // value computed independently from the contract side.
    const expected = Buffer.from([
      0, 0, 0, 18, 0, 0, 0, 0, 0, 0, 0, 0, 108, 63, 109, 157, 196, 54, 202,
      250, 175, 195, 191, 198, 149, 219, 42, 66, 108, 93, 226, 94, 186, 121,
      30, 231, 117, 212, 79, 204, 47, 209, 250, 132, 104, 101, 108, 108, 111,
      0, 0, 0, 0, 0, 1, 226, 64, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 15,
      66, 64,
    ]);

    assert.strictEqual(message.length, 73);
    assert.ok(message.equals(expected));
  });

  it("accepts number or bigint deadline/reward interchangeably", () => {
    const task = {
      owner: "GBWD63M5YQ3MV6VPYO74NFO3FJBGYXPCL25HSHXHOXKE7TBP2H5IICTP",
      calldata: Buffer.from("hello"),
      deadline: 123456n,
      reward: 1000000n,
    };
    const taskWithNumbers = { ...task, deadline: 123456, reward: 1000000 };

    assert.ok(
      buildSignatureVerifierMessage(task).equals(
        buildSignatureVerifierMessage(taskWithNumbers)
      )
    );
  });
});

describe("signProofForTask", () => {
  it("produces a verifiable 64-byte ed25519 signature over the task message", () => {
    const keypair = Keypair.random();
    const task = {
      owner: "GBWD63M5YQ3MV6VPYO74NFO3FJBGYXPCL25HSHXHOXKE7TBP2H5IICTP",
      calldata: Buffer.from("hello"),
      deadline: 123456n,
      reward: 1000000n,
    };

    const signature = signProofForTask(task, keypair);
    assert.strictEqual(signature.length, 64);

    const message = buildSignatureVerifierMessage(task);
    assert.ok(keypair.verify(message, signature));
  });

  it("produces different signatures for tasks with different identities", () => {
    const keypair = Keypair.random();
    const task = {
      owner: "GBWD63M5YQ3MV6VPYO74NFO3FJBGYXPCL25HSHXHOXKE7TBP2H5IICTP",
      calldata: Buffer.from("hello"),
      deadline: 123456n,
      reward: 1000000n,
    };
    const otherTask = { ...task, reward: 2000000n };

    const sig1 = signProofForTask(task, keypair);
    const sig2 = signProofForTask(otherTask, keypair);
    assert.ok(!sig1.equals(sig2));

    // sig1 must not verify against otherTask's message (replay check).
    const otherMessage = buildSignatureVerifierMessage(otherTask);
    assert.ok(!keypair.verify(otherMessage, sig1));
  });
});

describe("generateProof", () => {
  const eventTask = { taskId: 1, reward: 1000000n, deadline: 9999999999n };

  it("falls back to the placeholder proof when the task has no verifier", async () => {
    const fullTask = {
      owner: "GBWD63M5YQ3MV6VPYO74NFO3FJBGYXPCL25HSHXHOXKE7TBP2H5IICTP",
      calldata: Buffer.from("hello"),
      deadline: 9999999999n,
      reward: 1000000n,
      verifier: null,
    };
    const keypair = Keypair.random();

    const proof = await generateProof(eventTask, fullTask, keypair);
    // The placeholder path returns a hex-encoded string embedding the
    // task id, distinct from a raw-signature hex string in shape (it's
    // human-readable once decoded) — just confirm it's a hex string and
    // NOT the signature path's output.
    assert.strictEqual(typeof proof, "string");
    assert.match(proof, /^[0-9a-f]+$/);
    const decoded = Buffer.from(proof, "hex").toString("utf8");
    assert.match(decoded, /^keeper-proof:task:1:/);
  });

  it("falls back to the placeholder proof when no signing key is configured", async () => {
    const fullTask = {
      owner: "GBWD63M5YQ3MV6VPYO74NFO3FJBGYXPCL25HSHXHOXKE7TBP2H5IICTP",
      calldata: Buffer.from("hello"),
      deadline: 9999999999n,
      reward: 1000000n,
      verifier: "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAK3IM",
    };

    const proof = await generateProof(eventTask, fullTask, null);
    const decoded = Buffer.from(proof, "hex").toString("utf8");
    assert.match(decoded, /^keeper-proof:task:1:/);
  });

  it("produces a signature-based proof when the task has a verifier and a signing key is configured", async () => {
    const fullTask = {
      owner: "GBWD63M5YQ3MV6VPYO74NFO3FJBGYXPCL25HSHXHOXKE7TBP2H5IICTP",
      calldata: Buffer.from("hello"),
      deadline: 9999999999n,
      reward: 1000000n,
      verifier: "CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAK3IM",
    };
    const keypair = Keypair.random();

    const proof = await generateProof(eventTask, fullTask, keypair);
    const proofBytes = Buffer.from(proof, "hex");
    assert.strictEqual(proofBytes.length, 64);

    const message = buildSignatureVerifierMessage(fullTask);
    assert.ok(keypair.verify(message, proofBytes));
  });
});
