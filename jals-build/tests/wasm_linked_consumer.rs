//! A project compiled against a precompiled library, linked and run.
//!
//! The compiler twice: once for the library — [`CompileWasm::library`] — and once for the project
//! that links it, over an index that holds the library's published Java. The two modules then meet
//! in one `Store`, and the project's code calls into the library the way it would call into any
//! other class. This is the arrangement `jals.library` and [`LinkedLibrary`] exist for, at the
//! level a user reaches it: Java in, a linked program out.

#![cfg(feature = "wasm-run")]

use jals_hir::{FileAnalysis, FileId, FileSemantics, ProjectIndex, TypedFile};
use jals_javac::wasm::{CompileWasm, LinkedLibrary, Source, WasmOptions};
use std::io::Write as _;
use std::process::{Command, Stdio};

use tinywasm::{Imports, ModuleInstance, Store, WasmValue, parse_bytes};

/// Hand the module to `wasm-tools`, which is the specification's own answer to whether it is
/// well-formed; a host without the tool checks less, loudly.
fn validate(bytes: &[u8]) {
    let present = Command::new("wasm-tools")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    if !present {
        eprintln!(
            "note: `wasm-tools` is not installed; this test is checking less than it looks like"
        );
        return;
    }
    let mut child = Command::new("wasm-tools")
        .arg("validate")
        .arg("-")
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn wasm-tools");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(bytes)
        .expect("write module");
    let output = child.wait_with_output().expect("wasm-tools");
    assert!(
        output.status.success(),
        "wasm-tools rejected the module:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

const LIBRARY: &str = r"
package demo;

public class Counter {
    public static int TOTAL = 7;

    private int count;

    public Counter(int start) {
        this.count = start;
    }

    public int next() {
        this.count = this.count + 1;
        return this.count;
    }

    public static int twice(int n) {
        return n + n;
    }
}
";

const PROJECT: &str = r"
package app;

import demo.Counter;

public class Main {
    public static int run() {
        Counter.TOTAL = 9;
        Counter c = new Counter(40);
        return c.next() + Counter.twice(1) + Counter.TOTAL;
    }
}
";

/// Compile the library, then the project against it, and return `(library, project)` bytes.
fn compiled_pair(project_source: &str, library_sources: &[(&str, &str)]) -> (Vec<u8>, Vec<u8>) {
    let texts: Vec<&str> = std::iter::once(project_source)
        .chain(library_sources.iter().map(|(_, text)| *text))
        .collect();
    let roots: Vec<(FileId, jals_syntax::SyntaxNode)> = texts
        .iter()
        .enumerate()
        .map(|(index, text)| {
            (
                FileId(u32::try_from(index).expect("a source count that fits")),
                jals_exec::block_on_inline(jals_syntax::Parse::parse(text)).syntax(),
            )
        })
        .collect();
    let index = jals_exec::block_on_inline(ProjectIndex::builder(&roots).with_stdlib().build());
    let analyses: Vec<FileAnalysis> = roots
        .iter()
        .map(|(_, root)| jals_exec::block_on_inline(FileAnalysis::of(root)))
        .collect();
    let semantics: Vec<FileSemantics<'_>> = roots
        .iter()
        .zip(&analyses)
        .map(|((file, _), analysis)| analysis.in_project(&index, *file))
        .collect();
    let typed: Vec<TypedFile<'_>> = semantics
        .iter()
        .map(|binding| jals_exec::block_on_inline(binding.typed()))
        .collect();
    // The project wrote its source first, the library second; the split is the only thing the two
    // lists decide, and both compiles read the whole index for resolution.
    let (project, library) = typed.split_at(1);

    let (library_module, abi) = CompileWasm::library(
        library,
        &index,
        WasmOptions::default(),
        "lib",
        1,
        library_sources
            .iter()
            .map(|(path, text)| Source {
                path: (*path).to_owned(),
                text: (*text).to_owned(),
            })
            .collect(),
    )
    .expect("the library compiles");
    let linked = [LinkedLibrary {
        name: "lib",
        abi: &abi,
    }];
    let project_bytes =
        CompileWasm::project_linked(project, &[], &linked, &index, WasmOptions::default())
            .expect("the project compiles");

    let library_bytes = library_module.finish().expect("the library encodes");
    validate(&library_bytes);
    validate(&project_bytes);
    (library_bytes, project_bytes)
}

/// Compile the pair, link the two with the engine directly, and return what `run` answered.
fn linked_run(project_source: &str, library_sources: &[(&str, &str)]) -> i32 {
    let (library_bytes, project_bytes) = compiled_pair(project_source, library_sources);
    let library = parse_bytes(&library_bytes).expect("the library parses");
    let project = parse_bytes(&project_bytes).expect("the project parses");

    let mut store = Store::default();
    let library_instance =
        ModuleInstance::instantiate(&mut store, &library, None).expect("the library instantiates");
    let mut imports = Imports::new();
    imports
        .link_module("lib", library_instance)
        .expect("the library registers");
    let project_instance = ModuleInstance::instantiate(&mut store, &project, Some(&imports))
        .expect("the project links");

    let func = project_instance
        .func_untyped(&store, "run")
        .expect("the project exports `run`");
    let mut results = [WasmValue::I32(0)];
    func.call(&mut store, &[], &mut results)
        .expect("`run` executes");
    match results {
        [WasmValue::I32(value)] => value,
        other => panic!("expected one i32, got {other:?}"),
    }
}

/// A constructor the library exports is callable, a `static` field crosses in both directions,
/// and an instance method runs against an object the library allocated.
#[test]
fn a_project_calls_into_a_precompiled_library() {
    assert_eq!(linked_run(PROJECT, &[("demo/Counter.java", LIBRARY)]), 52);
}

/// A linked class the library gives an implicit constructor, and a constructor *reference* to it.
///
/// Both shapes missed the factory: the class declares no constructor, so the only construction
/// code is the synthesized initialiser, and `Made::new` reaches the same constructor through the
/// method-reference path rather than an explicit `new`. The value read back says whether the
/// initialiser ran — a factory that allocated without calling it leaves the default zero.
const IMPLICIT_LIBRARY: &str = r"
package demo;

public class Made {
    private int value = 41;

    public int read() {
        return this.value;
    }
}
";

const IMPLICIT_PROJECT: &str = r"
package app;

import demo.Made;

interface Maker {
    Made make();
}

public class Main {
    public static int run() {
        Maker maker = demo.Made::new;
        return maker.make().read();
    }
}
";

#[test]
fn a_constructor_reference_to_a_linked_class_runs_its_implicit_constructor() {
    assert_eq!(
        linked_run(IMPLICIT_PROJECT, &[("demo/Made.java", IMPLICIT_LIBRARY)]),
        41
    );
}

/// An inner class is reached through the outer instance the factory leads with.
///
/// The library's factory takes the enclosing instance as its first parameter — its in-module
/// constructor shape's parameter after `this` — and writes it into the synthetic field, because
/// the synthesized initialiser's own signature is the object alone. A consumer that emitted only
/// the declared arguments would call one short, and one that passed an enclosing instance to a
/// factory that did not want it left a value on the stack; both are modules no validator accepts.
const INNER_LIBRARY: &str = r"
package demo;

public class Outer {
    private int base;

    public Outer(int base) {
        this.base = base;
    }

    public class Inner {
        public int next() {
            return base + 1;
        }
    }
}
";

const INNER_PROJECT: &str = r"
package app;

import demo.Outer;

public class Main {
    public static int run() {
        Outer outer = new Outer(40);
        Outer.Inner inner = outer.new Inner();
        return inner.next();
    }
}
";

#[test]
fn a_project_constructs_a_linked_inner_class_through_its_outer_instance() {
    assert_eq!(
        linked_run(INNER_PROJECT, &[("demo/Outer.java", INNER_LIBRARY)]),
        41
    );
}

/// A project class extends a linked library's class.
///
/// The layout is split across the two modules and neither half is guessed: the replayed struct
/// carries the library's fields — synthetic entries and all — and they become the prefix the
/// project's own fields follow, which is what wasm's declared subtyping requires. Construction is
/// likewise two calls: the project's constructor runs, and its `super(x, y)` calls the library
/// constructor's *body* — exported under the factory's key with `#init` appended, because the
/// factory allocates and this object already exists. `tag` and `weight` are fields of the
/// subclass, so the answer also says the project's own slots landed *after* the library's rather
/// than on top of them.
const SUBCLASS_LIBRARY: &str = r"
package demo;

public class Point {
    protected int x;
    protected int y;

    public Point(int x, int y) {
        this.x = x;
        this.y = y;
    }

    public int sum() {
        return this.x + this.y;
    }
}
";

const SUBCLASS_PROJECT: &str = r"
package app;

import demo.Point;

public class Main {
    public static int run() {
        Tagged p = new Tagged(3, 4, 5);
        return p.sum() * 100 + p.tag() * 10 + p.weight();
    }
}

class Tagged extends Point {
    private int tag;
    private int weight = 2;

    Tagged(int x, int y, int tag) {
        super(x, y);
        this.tag = tag;
    }

    int tag() {
        return this.tag;
    }

    int weight() {
        return this.weight;
    }
}
";

#[test]
fn a_project_class_extends_a_linked_class_and_calls_super() {
    assert_eq!(
        linked_run(SUBCLASS_PROJECT, &[("demo/Point.java", SUBCLASS_LIBRARY)]),
        // 7 * 100 + 5 * 10 + 2
        752
    );
}

/// The same arrangement with no constructor written anywhere in the project or the middle of the
/// library's own chain.
///
/// `Middle` declares nothing and initialises nothing, so its own implicit constructor *is* its
/// ancestor's initialisers; the project subclass's implicit `super()` reaches that same export.
/// And `Mine` has one initialiser of its own, so its synthesized constructor has to run the
/// library chain first and then its own `own = 5` — the order JLS §12.5 requires.
const CHAIN_BASE: &str = r"
package demo;

public class Base {
    protected int base = 7;

    public int value() {
        return this.base;
    }
}
";

const CHAIN_MIDDLE: &str = r"
package demo;

public class Middle extends Base {
}
";

const CHAIN_PROJECT: &str = r"
package app;

import demo.Middle;

public class Main {
    public static int run() {
        Mine m = new Mine();
        return m.value() * 10 + m.own();
    }
}

class Mine extends Middle {
    private int own = 5;

    int own() {
        return this.own;
    }
}
";

#[test]
fn an_inherited_initialiser_runs_through_a_linked_chain() {
    assert_eq!(
        linked_run(
            CHAIN_PROJECT,
            &[
                ("demo/Base.java", CHAIN_BASE),
                ("demo/Middle.java", CHAIN_MIDDLE)
            ]
        ),
        // 7 * 10 + 5
        75
    );
}

/// A field a project class *inherits* from a linked class reports the missing capability rather
/// than an unresolved name.
///
/// The slot positions travelled with the replayed struct; the members deliberately did not, so
/// there is no wasm slot this module could name. Saying "`x` does not resolve" would send a reader
/// looking for a typo in legal Java — the same wrong-diagnostic the direct case was pinned
/// against, now reachable through a subclass.
#[test]
#[should_panic(expected = "an instance field of a linked library class")]
fn a_field_inherited_from_a_linked_class_is_refused_by_capability() {
    linked_run(
        r"
package app;

import demo.Point;

public class Main {
    public static int run() {
        Inside p = new Inside();
        return p.probe();
    }
}

class Inside extends Point {
    Inside() {
        super(0, 0);
    }

    int probe() {
        return this.x;
    }
}
",
        &[("demo/Point.java", SUBCLASS_LIBRARY)],
    );
}

/// An interface the library declares is a type the project can hold and call through.
///
/// wasm has no interface types: a `Greeter` here is `anyref`, and what makes the local legal — and
/// the call through it dispatch to the replayed `Shouter` — is the interface *name* in the ABI.
/// The project's index reads the library's published Java, so it resolves `Greeter#greet`; the
/// library's export list holds `Shouter#greet`; and the consumer's chain tests the concrete type
/// it replayed. Without the name, the local's type is a class with no struct, and the report is
/// "no wasm representation" for a class the library plainly declared.
///
/// The `(Greeter)` cast is the same fact from the other side: a cast to an interface is a cast to
/// what every reference already is, and `val_type` and the cast target have to agree.
///
/// `twice` is a `default` method, and it is the *member import* the interface list adds: an
/// abstract method has no export, but a default method has a function and the library exports it
/// like any other. The call from the project goes straight to it — the dispatch over the receiver
/// happens inside the library, where `Shouter` is a class its own compile replayed.
const INTERFACE_LIBRARY: &str = r"
package demo;

public interface Greeter {
    int greet();

    default int twice() {
        return this.greet() + this.greet();
    }
}
";

const INTERFACE_IMPLEMENTATION: &str = r"
package demo;

public class Shouter extends Object implements Greeter {
    public int greet() {
        return 41;
    }
}
";

const INTERFACE_PROJECT: &str = r"
package app;

import demo.Greeter;
import demo.Shouter;

public class Main {
    public static int run() {
        Object value = new Shouter();
        Greeter greeter = (Greeter) value;
        return greeter.greet() + greeter.twice();
    }
}
";

#[test]
fn a_project_holds_and_calls_through_a_linked_interface() {
    assert_eq!(
        linked_run(
            INTERFACE_PROJECT,
            &[
                ("demo/Greeter.java", INTERFACE_LIBRARY),
                ("demo/Shouter.java", INTERFACE_IMPLEMENTATION),
            ]
        ),
        123
    );
}

/// A `catch` catches the library's tag because the project *imports* it.
///
/// The project declares no tag of its own when a library exports one: two tags with the same
/// payload type are two different tags, and a `try_table` naming the local one would never see
/// the library's `throw`. Without the import the exception escapes the `try` and the call fails.
const SURPRISE_LIBRARY: &str = r"
package demo;

public class Surprise extends RuntimeException {
}
";

const THROWING_LIBRARY: &str = r"
package demo;

public class Thrower {
    public static void boom() {
        throw new Surprise();
    }
}
";

const CATCHING_PROJECT: &str = r"
package app;

import demo.Thrower;

public class Main {
    public static int run() {
        try {
            Thrower.boom();
            return 1;
        } catch (demo.Surprise e) {
            return 2;
        }
    }
}
";

#[test]
fn a_project_catches_an_exception_a_linked_library_throws() {
    assert_eq!(
        linked_run(
            CATCHING_PROJECT,
            &[
                ("demo/Surprise.java", SURPRISE_LIBRARY),
                ("demo/Thrower.java", THROWING_LIBRARY),
            ]
        ),
        2
    );
}

/// Legal Java this backend cannot compile yet reports what is missing.
///
/// The struct is shared, so the data is reachable — but the slots were laid out in the library's
/// own module, and deriving them from the replayed struct type would be inventing the declaration
/// order. The point of the test is the *diagnostic*: it used to say the field "did not resolve",
/// sending a reader looking for a typo in a program that is legal Java.
const FIELD_LIBRARY: &str = r"
package demo;

public class Point {
    public int x = 7;
}
";

const FIELD_PROJECT: &str = r"
package app;

import demo.Point;

public class Main {
    public static int run() {
        Point p = new Point();
        return p.x;
    }
}
";

#[test]
#[should_panic(expected = "an instance field of a linked library class")]
fn reading_a_linked_instance_field_names_the_missing_capability() {
    linked_run(FIELD_PROJECT, &[("demo/Point.java", FIELD_LIBRARY)]);
}

/// The same program through the runner a `jals run` uses: the bytes arrive as a `wasm` dependency
/// does, the library is instantiated first, and the project links against it by name.
#[test]
fn the_runner_links_a_wasm_dependency() {
    let (library_bytes, project_bytes) = compiled_pair(PROJECT, &[("demo/Counter.java", LIBRARY)]);
    let outcome = jals_build::WasmRunner::run(&jals_build::WasmRunRequest {
        module: &project_bytes,
        invoke: Some("run"),
        args: &[],
        natives: &jals_native::NativeBindings::new(),
        libraries: &[jals_build::WasmLibrary {
            name: "lib",
            bytes: &library_bytes,
        }],
        foreign: &[],
        progress: &jals_progress::Progress::SILENT,
    })
    .expect("the run links and executes");
    assert_eq!(
        outcome,
        jals_build::WasmRunOutcome::Returned(vec![jals_build::WasmValue::I32(52)])
    );
}

/// A library's **own** host imports are linked too, not only the project's.
///
/// A `native` method the library's Java declares is an import of the *library's* module, and the
/// project's import section says nothing about it — the project never has to call the method for
/// the declaration to be there. Before the sweep, the library could not be instantiated at all
/// (`linking error: unknown import: demo/Caller.answer()I`) even with the exact owner and
/// signature selected in `natives`.
const HOST_LIBRARY: &str = r"
package demo;

public class Caller {
    public static native int answer();

    public static int call() {
        return answer();
    }
}
";

const HOST_PROJECT: &str = r"
package app;

import demo.Caller;

public class Main {
    public static int run() {
        return Caller.call();
    }
}
";

#[test]
fn the_runner_links_a_library_host_imports() {
    let (library_bytes, project_bytes) =
        compiled_pair(HOST_PROJECT, &[("demo/Caller.java", HOST_LIBRARY)]);
    let mut registry = jals_native::NativeRegistry::new();
    let mut package = jals_native::NativePackage::new("demo.host", 1);
    package.bind(
        "demo/Caller",
        "answer()I",
        |_host: &mut dyn jals_native::NativeHost,
         _args: jals_native::Args<'_>,
         mut results: jals_native::Results<'_>| {
            results.set(0, jals_native::NativeValue::I32(42));
            Ok::<(), jals_native::NativeError>(())
        },
    );
    registry.add(package);
    let selected = registry
        .select(&["demo.host".to_owned()])
        .expect("the package selects");
    let outcome = jals_build::WasmRunner::run(&jals_build::WasmRunRequest {
        module: &project_bytes,
        invoke: Some("run"),
        args: &[],
        natives: &selected.bindings(),
        libraries: &[jals_build::WasmLibrary {
            name: "lib",
            bytes: &library_bytes,
        }],
        foreign: &[],
        progress: &jals_progress::Progress::SILENT,
    })
    .expect("the run links and executes");
    assert_eq!(
        outcome,
        jals_build::WasmRunOutcome::Returned(vec![jals_build::WasmValue::I32(42)])
    );
}

/// One frontend-published source, the shape a backend request takes.
fn backend_source(path: &str, text: &str) -> jals_build::BackendSource {
    let bytes = text.as_bytes().to_vec();
    jals_build::BackendSource {
        path: jals_storage::RelativePath::parse(path).expect("a valid path"),
        key: jals_storage::CacheKey::new(
            jals_storage::CacheNamespace::FrontendOutput,
            jals_storage::ContentDigest::of(b"test"),
            jals_storage::ContentDigest::of(&bytes),
        ),
        bytes,
    }
}

/// Compile `source` through the in-process wasm backend, with `selection`'s packages as its
/// natives and `libraries` linked, and return the outcome.
fn compile_against_packages(
    source: &str,
    selection: &jals_native::NativePackageSet,
    libraries: &[jals_build::BackendLibrary],
) -> jals_build::BackendOutcome {
    let tree = [backend_source("Main.java", source)];
    let options = jals_build::BackendOptions::default();
    let request = jals_build::BackendRequest {
        progress: &jals_progress::Progress::SILENT,
        tree: &tree,
        classpath: &[],
        libraries,
        options: &options,
    };
    let selected = jals_build::BackendSelection::in_process(
        jals_config::BackendKind::JalsWasm {},
        None,
        jals_build::Assertions::Disabled,
        selection.clone(),
    );
    let jals_build::BackendSelection::Available(backend) = selected else {
        panic!("the in-process wasm backend is available on every host");
    };
    jals_exec::block_on_inline(backend.compile(&request)).expect("the compile runs")
}

/// A package that ships its Java as a precompiled module instead of sources: the same program,
/// with the library arriving through `[build] native-packages` rather than a `wasm` dependency.
#[test]
fn a_package_can_ship_its_java_as_a_module() {
    let (library_bytes, _) = compiled_pair(PROJECT, &[("demo/Counter.java", LIBRARY)]);
    // A package's module is compiled into the binary that ships it, so its bytes are `'static`;
    // a test hands its own over the same way `include_bytes!` would.
    let library_bytes: &'static [u8] = Box::leak(library_bytes.into_boxed_slice());

    let mut package = jals_native::NativePackage::new("lib", 1);
    package.library(library_bytes);
    let mut registry = jals_native::NativeRegistry::new();
    registry.add(package);
    let selection = registry
        .select(&["lib".to_owned()])
        .expect("the package is registered");

    // What an editor or a linter indexes is the module's *published* Java: the package declares
    // no sources, so an editor reading `lowered_sources` would report every name from the
    // library unresolved.
    let published = jals_build::JalsBackend::native_package_sources(&selection);
    assert!(
        published
            .iter()
            .any(|(path, text)| path == "demo/Counter.java" && text.contains("class Counter")),
        "the module's Java is what a reader indexes: {published:?}"
    );

    let outcome = compile_against_packages(PROJECT, &selection, &[]);
    assert!(outcome.success(), "messages: {:?}", outcome.messages);
    let project_bytes = outcome
        .artifact(jals_build::JalsBackend::WASM_MODULE)
        .expect("the compile produced a module");

    let outcome = jals_build::WasmRunner::run(&jals_build::WasmRunRequest {
        module: project_bytes,
        invoke: Some("run"),
        args: &[],
        natives: &selection.bindings(),
        libraries: &[jals_build::WasmLibrary {
            name: "lib",
            bytes: library_bytes,
        }],
        foreign: &[],
        progress: &jals_progress::Progress::SILENT,
    })
    .expect("the run links and executes");
    assert_eq!(
        outcome,
        jals_build::WasmRunOutcome::Returned(vec![jals_build::WasmValue::I32(52)])
    );
}

/// A package's module that calls its **own** binding, through the `[build] native-packages` route.
///
/// The library's `native` method is an import of the *library's* module — the project never calls
/// it, and its import section says nothing about it. The package's binding table is what answers,
/// and only if the runner links host functions for the libraries it instantiates too. The
/// library class also declares no constructor, which exercises the synthesized factory such a
/// class gets: an extra value left on the stack is a module no validator accepts, and this route
/// validates the module before either half of it runs.
#[test]
fn a_package_ships_a_modules_own_native_methods_too() {
    let (library_bytes, _) = compiled_pair(HOST_PROJECT, &[("demo/Caller.java", HOST_LIBRARY)]);
    let library_bytes: &'static [u8] = Box::leak(library_bytes.into_boxed_slice());

    let mut package = jals_native::NativePackage::new("lib", 1);
    package.library(library_bytes);
    package.bind(
        "demo/Caller",
        "answer()I",
        |_host: &mut dyn jals_native::NativeHost,
         _args: jals_native::Args<'_>,
         mut results: jals_native::Results<'_>| {
            results.set(0, jals_native::NativeValue::I32(42));
            Ok::<(), jals_native::NativeError>(())
        },
    );
    let mut registry = jals_native::NativeRegistry::new();
    registry.add(package);
    let selection = registry
        .select(&["lib".to_owned()])
        .expect("the package is registered");

    let outcome = compile_against_packages(HOST_PROJECT, &selection, &[]);
    assert!(outcome.success(), "messages: {:?}", outcome.messages);
    let project_bytes = outcome
        .artifact(jals_build::JalsBackend::WASM_MODULE)
        .expect("the compile produced a module");

    let outcome = jals_build::WasmRunner::run(&jals_build::WasmRunRequest {
        module: project_bytes,
        invoke: Some("run"),
        args: &[],
        natives: &selection.bindings(),
        libraries: &[jals_build::WasmLibrary {
            name: "lib",
            bytes: library_bytes,
        }],
        foreign: &[],
        progress: &jals_progress::Progress::SILENT,
    })
    .expect("the run links and executes");
    assert_eq!(
        outcome,
        jals_build::WasmRunOutcome::Returned(vec![jals_build::WasmValue::I32(42)])
    );
}

/// The real platform, selected by name the way `[build] native-packages` would.
fn platform_selection() -> jals_native::NativePackageSet {
    let mut registry = jals_native::NativeRegistry::new();
    registry.add(jals_platform::Platform::package());
    registry
        .select(&[jals_platform::Platform::NAME.to_owned()])
        .expect("the platform is registered")
}

/// Run `project` against the real platform and return what `run()` answered.
fn run_against_platform(project: &str) -> jals_build::WasmRunOutcome {
    let selection = platform_selection();
    let outcome = compile_against_packages(project, &selection, &[]);
    assert!(outcome.success(), "messages: {:?}", outcome.messages);
    let project_bytes = outcome
        .artifact(jals_build::JalsBackend::WASM_MODULE)
        .expect("the compile produced a module");
    jals_build::WasmRunner::run(&jals_build::WasmRunRequest {
        module: project_bytes,
        invoke: Some("run"),
        args: &[],
        natives: &selection.bindings(),
        libraries: &[jals_build::WasmLibrary {
            name: jals_platform::Platform::NAME,
            bytes: jals_platform::Platform::MODULE,
        }],
        foreign: &[],
        progress: &jals_progress::Progress::SILENT,
    })
    .expect("the run links and executes")
}

/// `java.base` itself, through the route a build script reaches it by.
///
/// The platform is a package like any other: it ships a module, the Java it publishes travels in
/// that module's ABI section — so there is no host `java.base` anywhere in this test — and a
/// program that selects it links it by name. What this pins is that the platform is *sufficient*
/// for the code a script is made of: literals and the array constructor, the accessors, the builder
/// with its overloads, and the one crossing that is easy to get wrong, a `char[]` built by the
/// project into a constructor the library owns.
#[test]
fn the_platform_links_strings_and_builders() {
    let project = r#"
package app;

public class Main {
    public static int run() {
        String text = "hello";
        String greeting = text.concat(" world");
        String part = greeting.substring(0, 5);
        boolean same = part.equals(text);
        char[] chars = new char[3];
        chars[0] = 'a';
        chars[1] = 'b';
        chars[2] = 'c';
        String abc = new String(chars);
        chars[0] = 'z';
        StringBuilder builder = new StringBuilder(greeting);
        builder.append('/');
        builder.append(part);
        String built = builder.toString();
        int score = built.length() * 10000 + greeting.indexOf('w') * 100;
        if (same && abc.charAt(0) == 'a' && "abc".equals(abc)) {
            score = score + 11;
        }
        if (new String().isEmpty()) {
            score = score + 1;
        }
        return score;
    }
}
"#;

    let outcome = run_against_platform(project);
    // `built` is "hello world/hello" (17 code units), `indexOf('w')` is 6, the three equality
    // checks hold (including that copying the array kept `abc` at "abc" while `chars` became
    // "zbc"), and the empty string is empty: 170000 + 600 + 11 + 1.
    assert_eq!(
        outcome,
        jals_build::WasmRunOutcome::Returned(vec![jals_build::WasmValue::I32(170_612)])
    );
}

/// `+` with a `String` operand is not addition: it is a builder chain, and the builder — with the
/// rendering each overload gives its operand — is the platform's.
///
/// The expected string is written out and compared with `String.equals`, so a wrong rendering (a
/// `char` appended as its code point, a `long` truncated to `int`) fails on the text and not on a
/// length that happens to agree. `+=` goes through the same path, because it *is* `s = s + value`.
#[test]
fn the_platform_renders_concatenations() {
    let project = r#"
package app;

public class Main {
    public static int run() {
        String joined = "n=" + 7 + ", ok=" + true + ", c=" + 'x' + ", big=" + 9000000000L;
        String expected = "n=7, ok=true, c=x, big=9000000000";
        String message = "a";
        message += 'b';
        message += 12;
        int score = 0;
        if (joined.equals(expected)) {
            score = score + 1;
        }
        if (message.equals("ab12")) {
            score = score + 2;
        }
        return score;
    }
}
"#;

    let outcome = run_against_platform(project);
    // Both: the flattened chain rendered every operand by its own type, and `+=` built `"ab12"`.
    assert_eq!(
        outcome,
        jals_build::WasmRunOutcome::Returned(vec![jals_build::WasmValue::I32(3)])
    );
}

/// The boxes and the arithmetic, and the one thing a box is for on this target: a value of a
/// *supertype* whose method is implemented in the library.
///
/// Each check is worth one more bit than the last, so the seventeen of them answer `131071` — every
/// bit set — and a failure says by its arithmetic which check went wrong instead of only that one
/// did. The last two are the interesting ones: a `Number`-typed local dispatches to the override
/// replayed out of the library's ABI, and an `Object`-typed one reaches a `toString` that is also
/// a library function, which is what makes the boxes usable where an object is wanted.
#[test]
fn the_platform_boxes_and_computes() {
    let project = r#"
package app;

public class Main {
    public static int run() {
        int score = 0;

        Integer seven = Integer.valueOf(7);
        if (seven.intValue() == 7 && seven.toString().equals("7")) {
            score = score + 1;
        }
        if (seven.equals(Integer.valueOf(7)) && seven.hashCode() == 7) {
            score = score + 2;
        }

        Long big = Long.valueOf(9000000000L);
        if (big.longValue() == 9000000000L && big.toString().equals("9000000000")) {
            score = score + 4;
        }
        if (big.intValue() == 410065408) {
            score = score + 8;
        }

        Double half = Double.valueOf(2.5);
        if (half.doubleValue() == 2.5 && half.floatValue() == 2.5f && half.intValue() == 2) {
            score = score + 16;
        }
        if (Double.compare(0.0, -0.0) > 0 && Double.compare(-0.0, 0.0) < 0) {
            score = score + 32;
        }
        if (Double.compare(0.0 / 0.0, 1.0) > 0) {
            score = score + 64;
        }
        if (Double.valueOf(0.0 / 0.0).equals(Double.valueOf(0.0 / 0.0))) {
            score = score + 128;
        }

        Float tiny = Float.valueOf(0.5f);
        if (tiny.floatValue() == 0.5f && Float.compare(-0.0f, 0.0f) < 0) {
            score = score + 256;
        }

        if (Boolean.valueOf(true).toString().equals("true")) {
            score = score + 512;
        }
        if (Boolean.valueOf(false).toString().equals("false") && Boolean.valueOf(false).hashCode() == 1237) {
            score = score + 1024;
        }

        Character letter = Character.valueOf('x');
        if (letter.charValue() == 'x' && letter.toString().equals("x")) {
            score = score + 2048;
        }

        if (Math.max(2, 5) == 5 && Math.min(-1, 3) == -1 && Math.abs(-4) == 4) {
            score = score + 4096;
        }
        int mostNegative = -2147483647 - 1;
        if (Math.abs(mostNegative) == mostNegative) {
            score = score + 8192;
        }
        if (Math.sqrt(144.0) == 12.0 && Math.sqrt(-1.0) != Math.sqrt(-1.0)) {
            score = score + 16384;
        }

        Number boxed = Integer.valueOf(9);
        if (boxed.intValue() == 9 && boxed.longValue() == 9L) {
            score = score + 32768;
        }
        Object word = Integer.valueOf(4);
        if (word.toString().equals("4")) {
            score = score + 65536;
        }

        return score;
    }
}
"#;

    let outcome = run_against_platform(project);
    // Every check held: the boxes, the widenings and narrowings, the orders `Double.compare` and
    // `Float.compare` keep (NaN above every number and equal to itself, +0.0 above -0.0), the
    // builder behind the boxes' `toString`, Newton's `sqrt`, and the two dispatch checks.
    assert_eq!(
        outcome,
        jals_build::WasmRunOutcome::Returned(vec![jals_build::WasmValue::I32(131_071)])
    );
}

/// The exception classes are linkable platform classes, and one tag carries them all.
///
/// The project's `throw` and the platform's parser throw the *same* tag — the consumer imports the
/// one the library exports, because two tags with the same payload are still two tags — and the
/// class tests are `ref.test`s against the replayed structs, so the declared subtyping travels:
/// the object below is an `IllegalArgumentException` caught as a `RuntimeException`.
///
/// `toString` is a second virtual call, one level down: it is written once on `Throwable` and asks
/// `className()`, which each class in the platform answers for itself. The score is the message's
/// length (12) plus the rendered exception's (34 for the name, 2 for the separator, 12 for the
/// message), which says the message crossed the link and the name is the class that was thrown.
#[test]
fn the_platform_throws_and_catches() {
    let project = r#"
package app;

public class Main {
    public static int run() {
        int score = 0;
        try {
            throw new IllegalArgumentException("bad argument");
        } catch (RuntimeException e) {
            score = e.getMessage().length() + e.toString().length();
        }
        return score;
    }
}
"#;

    let outcome = run_against_platform(project);
    assert_eq!(
        outcome,
        jals_build::WasmRunOutcome::Returned(vec![jals_build::WasmValue::I32(60)])
    );
}

/// The same machinery a user reaches for: a class of the project's own that extends a platform
/// class, with the exception it names caught as the platform class above it.
///
/// `AppError` is laid out in the consumer, on top of the replayed `IllegalArgumentException`
/// struct, and its `super(message)` runs the library constructor's *body* — the `#init` export —
/// because the factory allocates and this object already exists. The catch tests the replayed
/// supertype, so the declared chain travels: `AppError` is caught as a `RuntimeException` and
/// `getMessage()` reads what the platform's constructor stored.
#[test]
fn a_project_exception_class_extends_the_platforms() {
    let project = r#"
package app;

public class Main {
    public static int run() {
        try {
            throw new AppError("nope");
        } catch (RuntimeException e) {
            return e.getMessage().length() * 10 + 1;
        }
    }
}

class AppError extends IllegalArgumentException {
    AppError(String message) {
        super(message);
    }
}
"#;

    let outcome = run_against_platform(project);
    // `"nope".length()` is 4, and the `+ 1` says the catch ran rather than an early return.
    assert_eq!(
        outcome,
        jals_build::WasmRunOutcome::Returned(vec![jals_build::WasmValue::I32(41)])
    );
}

/// The other direction of the same link: a call the *library* makes, answered by a class only the
/// consumer has.
///
/// `AppError` overrides `className`, and the platform's `Throwable.toString` — the body the
/// consumer imports — is where the call is made. The realm is the only thing that can answer it:
/// without one, the library's own `ref.test` chain finds the replayed `IllegalArgumentException`
/// *under* `AppError` (its struct extends it), calls the library's implementation and renders the
/// wrong name.
#[test]
fn a_library_call_dispatches_to_a_project_override() {
    let project = r#"
package app;

public class Main {
    public static int run() {
        try {
            throw new AppError("nope");
        } catch (RuntimeException e) {
            if (e.toString().equals("app.AppError: nope")) {
                return 7;
            }
            return 3;
        }
    }
}

class AppError extends IllegalArgumentException {
    AppError(String message) {
        super(message);
    }

    protected String className() {
        return "app.AppError";
    }
}
"#;

    let outcome = run_against_platform(project);
    assert_eq!(
        outcome,
        jals_build::WasmRunOutcome::Returned(vec![jals_build::WasmValue::I32(7)])
    );
}

/// `parseInt` and `parseLong` are the two parsers the platform could not have before the
/// exceptions existed: a parser that cannot report bad input has to invent an answer.
///
/// The checks are the corners — both ends of `int`, both of `long`, and three refusals: a digit
/// where there is none, an empty string, and `null`. The first refusal is caught as its exact
/// class, the second as `IllegalArgumentException` (declared subtyping again), and the third as
/// `NumberFormatException` — a null argument is bad input, not a crash.
#[test]
fn the_platform_parses_the_corners_and_refuses_bad_input() {
    let project = r#"
package app;

public class Main {
    public static int run() {
        int score = 0;
        if (Integer.parseInt("-2147483648") == -2147483647 - 1) {
            score = score + 1;
        }
        if (Integer.parseInt("2147483647") == 2147483647) {
            score = score + 2;
        }
        if (Long.parseLong("-9223372036854775808") == -9223372036854775807L - 1L) {
            score = score + 4;
        }
        if (Long.parseLong("9223372036854775807") == 9223372036854775807L) {
            score = score + 8;
        }
        try {
            Integer.parseInt("12x");
        } catch (NumberFormatException e) {
            if (e.getMessage().length() > 0) {
                score = score + 16;
            }
        }
        try {
            Long.parseLong("");
        } catch (IllegalArgumentException e) {
            score = score + 32;
        }
        try {
            Integer.parseInt(null);
        } catch (NumberFormatException e) {
            score = score + 64;
        }
        return score;
    }
}
"#;

    let outcome = run_against_platform(project);
    assert_eq!(
        outcome,
        jals_build::WasmRunOutcome::Returned(vec![jals_build::WasmValue::I32(127)])
    );
}

/// A `List` is a type a project holds, an interface it calls through, and a sequence it walks.
///
/// Every call in the test crosses the link twice over: the receiver is an interface — `List`,
/// `Collection`, `Iterable` — so the consumer's chain of `ref.test`s picks `ArrayList`, whose
/// methods are imports; and the elements are `String`s from the same platform, so `equals` and
/// `length` are imports too. What makes the chain work is the ABI's interface names: without them
/// the local's type has no representation, and with them the dispatch is the ordinary one — no
/// `$jals$link`, because every class that could answer was replayed from the library.
///
/// Each check is worth one more bit than the last, so all eight answer 255 and a failure says which
/// one by its arithmetic. They cover the growth path (`add` past ten elements), insertion in the
/// middle, `set` returning what was replaced, iteration to the end, and the two refusals the
/// platform throws: a negative capacity and a `get` past the end, caught by their own classes.
#[test]
fn the_platform_holds_a_list() {
    let project = r#"
package app;

import java.util.ArrayList;
import java.util.Iterator;
import java.util.List;

public class Main {
    public static int run() {
        int score = 0;

        List<String> names = new ArrayList<String>();
        names.add("moss");
        names.add("fern");
        names.add(1, "lichen");
        if (names.size() == 3 && !names.isEmpty()) {
            score = score + 1;
        }
        if (names.get(1).equals("lichen")) {
            score = score + 2;
        }
        String replaced = names.set(0, "stone");
        if (replaced.equals("moss") && names.get(0).equals("stone")) {
            score = score + 4;
        }

        Iterator<String> it = names.iterator();
        int total = 0;
        while (it.hasNext()) {
            String name = it.next();
            total = total + name.length();
        }
        if (total == 15) {
            score = score + 8;
        }

        names.clear();
        if (names.isEmpty() && names.size() == 0) {
            score = score + 16;
        }

        try {
            new ArrayList<String>(-1);
        } catch (IllegalArgumentException e) {
            score = score + 32;
        }
        try {
            names.get(5);
        } catch (IndexOutOfBoundsException e) {
            score = score + 64;
        }

        List<String> many = new ArrayList<String>(1);
        int i = 0;
        while (i < 20) {
            many.add("x");
            i = i + 1;
        }
        if (many.size() == 20) {
            score = score + 128;
        }

        return score;
    }
}
"#;

    let outcome = run_against_platform(project);
    assert_eq!(
        outcome,
        jals_build::WasmRunOutcome::Returned(vec![jals_build::WasmValue::I32(255)])
    );
}

/// An element-comparing method the platform leaves out is a compile error, not a wrong answer.
///
/// `contains` would have to run `equals` on an element that may be a consumer's object, and a
/// library body cannot dispatch a method on a consumer type yet. The method is therefore not
/// declared, and the call is refused while the source is in hand — the alternative, comparing with
/// `==`, would be false for two equal strings and true only by accident of identity (`ArrayList`'s
/// element slots are erased, and a string read off one heap is not the same object on another).
#[test]
fn a_container_method_the_platform_leaves_out_is_refused_by_name() {
    let project = r#"
package app;

import java.util.ArrayList;
import java.util.List;

public class Main {
    public static int run() {
        List<String> names = new ArrayList<String>();
        names.add("moss");
        if (names.contains("moss")) {
            return 1;
        }
        return 0;
    }
}
"#;

    let selection = platform_selection();
    let outcome = compile_against_packages(project, &selection, &[]);
    assert!(
        !outcome.success(),
        "`contains` is not declared, so the call cannot resolve: {:?}",
        outcome.messages
    );
    assert!(
        outcome
            .messages
            .iter()
            .any(|message| message.contains("contains")),
        "the report names the method that was asked for: {:?}",
        outcome.messages
    );
}

/// The overload a concatenation operand names has to exist, and the report says which one is
/// missing.
///
/// A `double` names `append(double)`, which the platform has not written yet, and the JDK's
/// fallback — boxing into `append(Object)` — is exactly what §15.18.1 says *not* to do: `"" + d`
/// renders the decimal text, not the identity of a `Double`. So the call is refused with the
/// method it asked for, which is a compile error at the `+`, rather than a rendering that would be
/// almost right.
#[test]
fn a_concatenation_names_the_overload_the_platform_must_have() {
    let project = r#"
package app;

public class Main {
    public static int run() {
        double ratio = 1.5;
        String text = "ratio=" + ratio;
        return text.length();
    }
}
"#;

    let selection = platform_selection();
    let outcome = compile_against_packages(project, &selection, &[]);
    assert!(
        !outcome.success(),
        "the platform has no `append(double)` yet"
    );
    assert!(
        outcome
            .messages
            .iter()
            .any(|message| message.contains("java.lang.StringBuilder.append(double)")),
        "the report names the overload it asked for: {:?}",
        outcome.messages
    );
}

/// One link name cannot be two modules: a `wasm` dependency and a selected package that agree on
/// a name are refused while both halves are still in hand, not at instantiation in the engine's
/// vocabulary.
#[test]
fn a_wasm_dependency_and_a_package_cannot_share_a_link_name() {
    let (library_bytes, _) = compiled_pair(PROJECT, &[("demo/Counter.java", LIBRARY)]);
    let library_bytes: &'static [u8] = Box::leak(library_bytes.into_boxed_slice());

    let mut package = jals_native::NativePackage::new("lib", 1);
    package.library(library_bytes);
    let mut registry = jals_native::NativeRegistry::new();
    registry.add(package);
    let selection = registry
        .select(&["lib".to_owned()])
        .expect("the package is registered");

    let abi = jals_build::LibraryAbi::of_module(library_bytes).expect("a linked library");
    let dependency = [jals_build::BackendLibrary {
        name: "lib".to_owned(),
        abi,
    }];
    let outcome = compile_against_packages(PROJECT, &selection, &dependency);
    assert!(!outcome.success(), "the compile refuses the collision");
    assert!(
        outcome
            .messages
            .iter()
            .any(|message| message
                .contains("both a `wasm` dependency and a selected native package")),
        "messages: {:?}",
        outcome.messages
    );
}

/// A string literal is built by a package, not by the compiler: the characters travel as a passive
/// data segment, `array.new_data` copies them into a `char[]`, and `java.lang.String`'s own
/// constructor turns that into an object.
///
/// The library here is a *fragment* of java.base — `String` alone — which is the shape the real
/// platform has: the compiler holds no built-in `String`, and the stub `java.lang.String` in the
/// index is shadowed by the library's published source, so the literal, the class, and the array
/// type all resolve to the same declarations on both sides of the link. The characters picked here
/// say whether the data segment is read at all: `length` would be 0 from an empty array, and an
/// off-by-one in the copy would show as the wrong code unit.
const STRING_LIBRARY: &str = r"
package java.lang;

public class String {
    private char[] value;

    public String(char[] value) {
        this.value = value;
    }

    public int length() {
        return this.value.length;
    }

    public char charAt(int index) {
        return this.value[index];
    }
}
";

const STRING_PROJECT: &str = r#"
package app;

public class Main {
    public static int run() {
        String text = "a\tb";
        return text.length() * 100 + text.charAt(1);
    }
}
"#;

/// `"a\tb"` is three code units, and the second is a tab — 9.
#[test]
fn a_string_literal_is_built_from_module_data_by_the_linked_package() {
    assert_eq!(
        linked_run(STRING_PROJECT, &[("java/lang/String.java", STRING_LIBRARY)]),
        309
    );
}

/// The library's own code uses a literal the same way, except that *it* holds the class: its
/// constructor is an in-module function taking a receiver, not a factory import, so the object is
/// allocated here and the copied `char[]` pushed underneath it. The string then crosses the
/// boundary as an ordinary replayed reference, which is what makes a library-built `String` usable
/// by the project that links it.
const GREETING_LIBRARY: &str = r#"
package demo;

public class Greeting {
    public static String text() {
        return "ok";
    }
}
"#;

const GREETING_PROJECT: &str = r"
package app;

import demo.Greeting;

public class Main {
    public static int run() {
        return Greeting.text().length() * 10 + Greeting.text().charAt(0);
    }
}
";

/// `"ok"` is two code units and the first is `o` — 111.
#[test]
fn a_library_builds_its_own_literals_and_hands_the_string_across_the_link() {
    assert_eq!(
        linked_run(
            GREETING_PROJECT,
            &[
                ("java/lang/String.java", STRING_LIBRARY),
                ("demo/Greeting.java", GREETING_LIBRARY),
            ]
        ),
        131
    );
}
