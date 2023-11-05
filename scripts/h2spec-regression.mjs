import { Octokit } from "octokit";
import { XMLParser } from "fast-xml-parser";
import fs from "fs/promises";

/**
 * @typedef {Object} Spec - Specification to check
 * @property {string} junitPath - Path to the JUnit XML file
 * @property {string} checkName - Name of the check to compare against
 */

/**
 * @typedef {Object} Results - Results of a test run
 * @property {number} total - Total number of tests
 * @property {number} passed - Number of tests passed
 * @property {number} failed - Number of tests failed
 * @property {number} skipped - Number of tests skipped
 */

let octokit = new Octokit({
  auth: process.env.TOKEN,
});

let checks = await octokit.rest.checks.listForRef({
  owner: "hapsoc",
  repo: "fluke",
  ref: "main",
});

let xmlParser = new XMLParser({
  ignoreAttributes: false,
});

/**
 * @param {string} junitPath - Path to the JUnit XML file
 */
let getCurrentResults = async (junitPath) => {
  let doc = xmlParser.parse(await fs.readFile(junitPath));

  /**
   * @type {Results}
   */
  let results = {
    total: 0,
    passed: 0,
    failed: 0,
    skipped: 0,
  };

  for (const ts of doc.testsuites.testsuite) {
    let tests = parseInt(ts["@_tests"], 10);
    let failures = parseInt(ts["@_failures"], 10);
    let errors = parseInt(ts["@_errors"], 10);
    let skipped = parseInt(ts["@_skipped"], 10);

    results.total += tests;
    results.failed += failures + errors;
    results.skipped += skipped;
    results.passed += tests - (failures + errors + skipped);
  }

  return results;
};

/**
 * Get the results of the last run of h2spec on the `main` branch
 * @param {string} checkName
 * @returns {Results}
 */
let getReferenceResults = async (checkName) => {
  let http2Check = checks.data.check_runs.find((c) => c.name == checkName);
  if (!http2Check) {
    console.log("No h2spec-http2 check found");
    process.exit(0);
  }

  // example input:
  //   **94** tests were completed in **NaNms** with **70** passed, **24** failed and **0** skipped
  // example output:
  //   { total: 94, passed: 70, failed: 24, skipped: 0 }
  const regex =
    /[*][*](\d+)[*][*] tests were completed in [*][*](\w+)ms[*][*] with [*][*](\d+)[*][*] passed, [*][*](\d+)[*][*] failed and [*][*](\d+)[*][*] skipped/g;
  let m = regex.exec(http2Check.output.summary);
  return {
    total: parseInt(m[1], 10),
    passed: parseInt(m[3], 10),
    failed: parseInt(m[4], 10),
    skipped: parseInt(m[5], 10),
  };
};

/**
 * @type {Spec[]}
 */
let specs = [
  {
    junitPath: "target/h2spec-generic.xml",
    checkName: "h2spec-generic",
  },
  {
    junitPath: "target/h2spec-hpack.xml",
    checkName: "h2spec-hpack",
  },
  {
    junitPath: "target/h2spec-http2.xml",
    checkName: "h2spec-http2",
  },
];

let regressionsDetected = false;
let outputLines = [];

for (const spec of specs) {
  let current = await getCurrentResults(spec.junitPath);
  let reference = await getReferenceResults(spec.checkName);
  if (current.failed > reference.failed) {
    outputLines.push(
      `Regression detected in ${spec.checkName}: ${current.failed} > ${reference.failed}`,
    );
    regressionsDetected = true;
  } else {
    let diff;
    if (current.failed == reference.failed) {
      diff = "unchanged";
    } else if (current.failed < reference.failed) {
      diff = `-${reference.failed - current.failed}`;
    } else if (current.failed > reference.failed) {
      diff = `+${current.failed - reference.failed}`;
    }

    outputLines.push(
      `No regression in ${spec.checkName}: failed count ${diff} (${current.passed} passed, ${current.failed} failed)`,
    );
  }
}

if (regressionsDetected) {
  outputLines.push(`Regressions detected, failing the build`);
  process.exit(1);
}

// Leave a comment on the PR with all lines in outputLines
let comment = outputLines.join("\n");

let github_ref = process.env.GITHUB_REF || "";
{
  let m = /refs\/pull\/(\d+)\/merge/g.exec(github_ref);
  if (m) {
    let pr_number = m[1];
    console.log(`Leaving comment on PR #${pr_number}`);
    await octokit.rest.issues.createComment({
      owner: "hapsoc",
      repo: "fluke",
      issue_number: process.env.PR_NUMBER,
      body: comment,
    });
  } else {
    console.log(
      `Not a PR, not leaving a comment. Comment would've been:\n${comment}`,
    );
  }
}
