/* eslint-env node */

module.exports = {
  root: true,
  parser: "@typescript-eslint/parser",
  plugins: ["@typescript-eslint"],
  extends: [
    "eslint:recommended",
    "plugin:@typescript-eslint/eslint-recommended",
    "plugin:@typescript-eslint/recommended",
  ],
  // check using: DEBUG=eslint:cli-engine yarn run lint
  ignorePatterns: ["node_modules/", "dist/", "jest.config.cjs"],
};
