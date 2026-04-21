// WHY: @babel/plugin-syntax-jsx ships no TypeScript declarations and no
// @types/babel__plugin-syntax-jsx exists on DefinitelyTyped. Declaring the
// module here satisfies tsc without pinning to `any` across the whole project.
// The plugin is used only in compiler-canary.test.ts as a Babel plugin handle;
// typed as `object` to match @types/babel__core's PluginTarget union, which
// is what babel.transformAsync's `plugins` array ultimately accepts.
declare module "@babel/plugin-syntax-jsx" {
  const plugin: object;
  export default plugin;
}
