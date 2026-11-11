import { runSuite, runTestDynamic } from "./WebCryptoAPI.harness.js";

runSuite(import.meta.url, runTestDynamic, [], {
  tentativeFiles: ["mldsa.tentative.https.any.js"],
});
