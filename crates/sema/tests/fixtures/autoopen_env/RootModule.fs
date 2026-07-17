// A genuine ROOT-namespace module — it needs its own file, since a file that opens with
// `namespace` cannot also declare a global module.
//
// For the anonymous-root collision (Slice A review, round 4): a headerless project file
// can declare `module RootOpened` whose values sema cannot enumerate (an anonymous-root
// nested module carries no qualified export path), while this assembly exports a module
// of the *same* path. FCS opens both and binds the LOCAL one, so an assembly module at
// the written path must not suppress the project-opaque fallback.
module RootOpened

let rootShared (x: int) = x + 60
