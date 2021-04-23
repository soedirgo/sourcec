# sourcec: Source §1 to LLVM IR compiler

This compiler takes a Source §1 program represented in ESTree JSON format as input and produces LLVM IR as output, which you can run in e.g. [llvm-wasm](https://soedirgo.github.io/llvm-wasm/).

## Prerequisites
#### Install LLVM 11
If you are on macOS, you can run the following using `brew`:
```
brew install llvm@11
```
If you are on Linux, you can follow the instructions [here](https://apt.llvm.org). Make sure to choose LLVM version 11.
#### Install the Rust toolchain
Follow the instructions [here](https://www.rust-lang.org/tools/install). This project uses Rust stable 1.51, but later versions should work fine.
#### Install NodeJS & Yarn
## Usage
We'll be using the following Source program as example:
```js
// main.js
function fib(n) {
    if (n <= 1) {
        return n;
    } else {
        return fib(n - 1) + fib(n - 2);
    }
}
fib(10);
```

First, `cd` to `scripts` and do a `yarn install`.

To get the ESTree JSON representation of the Source program, `cd` back into the project root and run:

```js
cat main.js | scripts/parse
```

Then we can pass it to `sourcec` to get the LLVM IR:

```js
cat main.js | scripts/parse | cargo run > main.ll
```

You can now copy the contents of `main.ll` and run it on e.g. [llvm-wasm](https://soedirgo.github.io/llvm-wasm/). Note that the `.ll` module is set to target `wasm32-unknown-wasi` and a particular target data layout to ensure 32 bit pointer size, so you'll need more work to run it directly on your machine with e.g. `lli` or `llc`.
## Developing
sourcec compiles a subset of Source §1 which is specified [here](https://github.com/soedirgo/sourcec/blob/main/source_1_sourcec.pdf).

Testing was done in an ad-hoc manner. For an extensive test suite/example programs you might want to check out [llvm-sauce](https://github.com/jiachen247/llvm-sauce).

The repo is structured like so:

```
.
├── scripts
│   └── parse        // parses a Source program to its ESTree representation, uses Yarn & NodeJS
└── src
    ├── env.rs       // compile-time environment logic
    ├── expr.rs      // handles compilation of expressions
    ├── helper.rs    // contains helper functions for building literals, allocation, etc.
    ├── lib.rs       // entry point of compilation logic, exposes `fn compile(&str)`
    ├── main.rs      // simple runner for reading from stdin and writing to stdout
    └── stmt.rs      // handles compilation of statements
```
