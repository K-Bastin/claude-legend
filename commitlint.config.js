export default {
  extends: ["@commitlint/config-conventional"],
  rules: {
    "scope-enum": [1, "always", ["pty", "sync", "sessions", "ui", "config", "ci", "deps", "release"]],
  },
};
