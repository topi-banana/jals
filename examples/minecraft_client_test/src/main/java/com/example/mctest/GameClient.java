package com.example.mctest;

// Every declaration in this file is `#[cfg(feature = "enabled")]`, imports included — the release
// this harness is written against, and the only feature it declares.
//
// Two things need the gate. Without the release the SDK dependency resolves no client jar at all,
// so `net.minecraft.client.*` names nothing; and a consumer that declares this project in
// `[dev-dependencies]` still discovers it under every *other* selection, because a dependency is a
// node whether or not the selection routes a feature to it. A `cfg`-disabled declaration is blanked
// before anything tries to resolve it, so under those selections this file lowers to its `package`
// line and nothing else — a compilation unit that declares no type and emits no class.
//
// Java allows no annotation on an import, but the jals dialect's `#[cfg]` is not an annotation:
// `jals-syntax` treats an import as an attribute host like any other declaration.
#[cfg(feature = "enabled")] import java.io.IOException;
#[cfg(feature = "enabled")] import java.io.UncheckedIOException;
#[cfg(feature = "enabled")] import java.lang.management.ManagementFactory;
#[cfg(feature = "enabled")] import java.nio.charset.StandardCharsets;
#[cfg(feature = "enabled")] import java.nio.file.Files;
#[cfg(feature = "enabled")] import java.nio.file.Path;
#[cfg(feature = "enabled")] import java.nio.file.Paths;
#[cfg(feature = "enabled")] import java.time.Duration;
#[cfg(feature = "enabled")] import java.util.Comparator;
#[cfg(feature = "enabled")] import java.util.List;
#[cfg(feature = "enabled")] import java.util.concurrent.CompletableFuture;
#[cfg(feature = "enabled")] import java.util.concurrent.ExecutionException;
#[cfg(feature = "enabled")] import java.util.concurrent.Executor;
#[cfg(feature = "enabled")] import java.util.concurrent.TimeUnit;
#[cfg(feature = "enabled")] import java.util.concurrent.TimeoutException;
#[cfg(feature = "enabled")] import java.util.function.Predicate;
#[cfg(feature = "enabled")] import java.util.function.Supplier;
#[cfg(feature = "enabled")] import java.util.stream.Collectors;
#[cfg(feature = "enabled")] import java.util.stream.Stream;
#[cfg(feature = "enabled")] import net.minecraft.client.Minecraft;
#[cfg(feature = "enabled")] import net.minecraft.client.gui.components.AbstractWidget;
#[cfg(feature = "enabled")] import net.minecraft.client.gui.components.events.GuiEventListener;
#[cfg(feature = "enabled")] import net.minecraft.client.gui.screens.Overlay;
#[cfg(feature = "enabled")] import net.minecraft.client.gui.screens.Screen;
#[cfg(feature = "enabled")] import net.minecraft.client.gui.screens.TitleScreen;
#[cfg(
    all(
        feature = "enabled", feature = "since-1.16",
        not(
            feature = "since-1.19")))] import net.minecraft.client.gui.screens.worldselection.WorldPreset;
#[cfg(
    all(
        feature = "enabled", feature = "since-1.19",
        not(feature = "since-1.19.3")))] import net.minecraft.core.Registry;
#[cfg(
    all(
        feature = "enabled", feature = "since-1.16",
        not(feature = "since-1.19.3")))] import net.minecraft.core.RegistryAccess;
#[cfg(
    all(
        feature = "enabled", feature = "since-1.19.3",
        not(feature = "since-1.21.2")))] import net.minecraft.core.registries.Registries;
#[cfg(feature = "enabled")] import net.minecraft.server.MinecraftServer;
#[cfg(feature = "enabled")] import net.minecraft.server.level.ServerLevel;
#[cfg(all(feature = "enabled", feature = "since-1.16"))] import net.minecraft.world.Difficulty;
#[cfg(
    all(
        feature = "enabled", feature = "since-1.16",
        not(feature = "since-1.19.3")))] import net.minecraft.world.level.DataPackConfig;
#[cfg(
    all(
        feature = "enabled", feature = "since-1.16",
        not(feature = "since-1.21.11")))] import net.minecraft.world.level.GameRules;
#[cfg(feature = "enabled")] import net.minecraft.world.level.GameType;
#[cfg(feature = "enabled")] import net.minecraft.world.level.LevelSettings;
#[cfg(
    all(
        feature = "enabled",
        not(feature = "since-1.16")))] import net.minecraft.world.level.LevelType;
#[cfg(
    all(
        feature = "enabled",
        feature = "since-1.19.3"))] import net.minecraft.world.level.WorldDataConfiguration;
#[cfg(
    all(
        feature = "enabled",
        not(feature = "since-1.16")))] import net.minecraft.world.level.dimension.DimensionType;
#[cfg(
    all(
        feature = "enabled", feature = "since-1.21.11",
        not(feature = "since-26.1")))] import net.minecraft.world.level.gamerules.GameRules;
#[cfg(
    all(
        feature = "enabled",
        feature = "since-1.19.3"))] import net.minecraft.world.level.levelgen.WorldOptions;
#[cfg(
    all(
        feature = "enabled", feature = "since-1.19.3",
        not(
            feature = "since-1.21.2")))] import net.minecraft.world.level.levelgen.presets.WorldPreset;
#[cfg(
    all(
        feature = "enabled",
        feature = "since-1.19"))] import net.minecraft.world.level.levelgen.presets.WorldPresets;

/**
 * A Minecraft client, booted in this JVM and driven from a {@code #[test]} method.
 *
 * <p>There is no Mixin here, no java agent and no launcher. The whole hinge is that {@code
 * Minecraft} implements {@link Executor}: {@code execute(Runnable)} overrides a JDK interface
 * method, so no mapping set may rename it, and a thread that is not the render thread can put work
 * on the render thread through it. {@link #evalOnClient} is that call with a result and an
 * exception path attached; {@link #evalOnServer} is the same thing against {@link MinecraftServer},
 * which is an {@code Executor} for the same reason.
 *
 * <p>The game runs on a <em>daemon</em> thread and this object never asks it to stop. That is
 * deliberate. {@code jals test} reads a test as passed when the generated harness prints its
 * sentinel line, which happens after the test method returns; a client that took the JVM down on
 * its way out would take the sentinel with it. So the test abandons the game instead, and {@link
 * #close()} arms a watchdog that halts the JVM once the sentinel has had its moment — the client
 * leaves non-daemon worker threads behind, so without it the process would simply never exit.
 *
 * <p>That watchdog halts with status <b>0</b>, which is only safe while {@code jals test} is
 * reading the sentinel. Under {@code --no-capture} there is no captured output to read and the
 * runner falls back to the exit status, so a client test that <em>failed</em> would be reported as
 * passed. Do not run this harness with {@code --no-capture}.
 *
 * <p>Linux only. GLFW wants the main thread on macOS ({@code -XstartOnFirstThread}), and the main
 * thread belongs to the test.
 */
#[cfg(feature = "enabled")]
public final class GameClient implements AutoCloseable {
    /** Where the client's `saves/`, `logs/` and `options.txt` go. */
    private static final String RUN_ROOT = "target/jals/build/mc-client";

    /**
     * The asset index this harness writes, with no objects in it.
     *
     * <p>A launcher points the client at a store of a few thousand downloaded objects. Almost all
     * of them are sounds and translations; the textures, models and shaders a boot actually needs
     * are inside the client jar. So the store is left empty and named by this harness rather than
     * by the release, which is one fewer constant to update.
     */
    private static final String ASSET_INDEX = "jals-empty";

    /**
     * The seed the releases that take one are given.
     *
     * <p>Fixed rather than random, so a failure on an old release is reproducible. A superflat world
     * looks the same whatever it is — the seed decides where the structures go, and this world has
     * none.
     *
     * <p>Every era takes it, which is why it is one constant and not four spellings.
     * {@code WorldOptions} has convenience factories, but *which* ones is release-specific
     * ({@code testWorldWithRandomSeed} arrived at 1.21.2, {@code defaultWithRandomSeed} at 1.19.3),
     * while its {@code (seed, structures, bonusChest)} constructor is the same on all of them.
     */
    private static final long FLAT_SEED = 0L;

    private static final Duration BOOT_DEADLINE = Duration.ofSeconds(300);
    private static final Duration STEP_DEADLINE = Duration.ofSeconds(60);
    private static final Duration WORLD_DEADLINE = Duration.ofSeconds(300);
    private static final long POLL_MILLIS = 50L;

    /**
     * How long one {@code get} inside {@link #evalOn} waits before the liveness check runs again.
     *
     * <p>Not a deadline of its own: the loop keeps waiting until the caller's own deadline is up.
     * It only bounds how long a wait can go on after the game thread has died.
     */
    private static final long LIVENESS_POLL_NANOS = POLL_MILLIS * 1_000_000L;

    /**
     * How long the JVM is given to print the harness sentinel and shut down on its own before the
     * watchdog stops waiting. Generous: the sentinel is one {@code println} away.
     */
    private static final Duration HALT_AFTER = Duration.ofSeconds(20);

    private final Minecraft client;
    private final Thread game;
    private final Path directory;

    /** Whatever killed the game thread, so a wait that fails reports the cause and not the clock. */
    private volatile Throwable failure;

    private GameClient(Minecraft client, Thread game, Path directory) {
        this.client = client;
        this.game = game;
        this.directory = directory;
    }

    /** Boot a client and return once the title screen is up. */
    public static GameClient launch() {
        Path directory = Paths.get(RUN_ROOT, jvmIdentity()).toAbsolutePath();
        try {
            GameClient game = start(directory);
            game.awaitTitleScreen();
            return game;
        } catch (RuntimeException | Error thrown) {
            // A boot that failed leaves the same JVM behind as one that succeeded — the game
            // thread is running and whatever it got as far as constructing has non-daemon workers
            // behind it. Nothing calls `close()` on an instance that was never returned, so the
            // watchdog is armed here instead; without it the failure is reported and the process
            // then hangs until `--timeout` kills it, or forever when none was given.
            armHalt();
            throw thrown;
        }
    }

    /** The run directory this client owns. Kept after the run, so a failure leaves its log behind. */
    public Path directory() {
        return this.directory;
    }

    /** The client itself. Reading anything off it is only safe on the render thread. */
    public Minecraft client() {
        return this.client;
    }

    // --- running work on the two game threads --------------------------------------------------

    /** Run {@code action} on the render thread and return what it produced. */
    public <T> T evalOnClient(GameAction<Minecraft, T> action) {
        return evalOn(this.client, this.client, action, "the render thread", STEP_DEADLINE);
    }

    /** Run {@code action} on the render thread. */
    public void runOnClient(GameEffect<Minecraft> action) {
        runOnClient(action, STEP_DEADLINE);
    }

    /**
     * The same, for a body the render thread runs for longer than a step.
     *
     * <p>The deadline is how long the <em>caller</em> waits, and a body that generates a world is
     * not a body that reads a field: giving both the same budget makes the slower one time out
     * while it is still running, which reads as a harness failure and is not one.
     */
    private void runOnClient(GameEffect<Minecraft> action, Duration deadline) {
        evalOn(
            this.client,
            this.client,
            client -> {
                action.accept(client);
                return null;
            },
            "the render thread",
            deadline);
    }

    /** Run {@code action} on the integrated server's thread and return what it produced. */
    public <T> T evalOnServer(GameAction<MinecraftServer, T> action) {
        MinecraftServer server = server();
        return evalOn(server, server, action, "the server thread", STEP_DEADLINE);
    }

    /** Run {@code action} on the integrated server's thread. */
    public void runOnServer(GameEffect<MinecraftServer> action) {
        evalOnServer(
            server -> {
                action.accept(server);
                return null;
            });
    }

    /**
     * The running integrated server.
     *
     * <p>Vanilla publishes no static accessor for a {@code MinecraftServer} — this is the one that
     * exists, and it is the reason a test that wants to inspect a world boots a <em>client</em>.
     */
    /**
     * The overworld of the running integrated server.
     *
     * <p>Published so a consumer's test does not have to know that 1.16 replaced
     * {@code getLevel(DimensionType.OVERWORLD)} with an {@code overworld()} of its own.
     */
    public ServerLevel overworld() {
        return evalOnServer(GameClient::overworld);
    }

    public MinecraftServer server() {
        MinecraftServer server = evalOnClient(Minecraft::getSingleplayerServer);
        if (server == null) {
            throw new GameFailure("no integrated server is running; open a world first", null);
        }
        return server;
    }

    // --- waiting -------------------------------------------------------------------------------

    /**
     * Poll {@code condition} on the render thread until it holds.
     *
     * <p>There is no sleeping for a fixed interval anywhere in this class. A boot on a software
     * rasterizer takes as long as it takes, and a number picked to cover the slowest machine is a
     * number every faster one pays.
     */
    public void waitUntil(String what, Predicate<Minecraft> condition, Duration deadline) {
        pollOnClient(what, client -> condition.test(client) ? Boolean.TRUE : null, deadline);
    }

    /**
     * Wait until the showing screen is a {@code type}, and return it.
     *
     * <p>Tested and captured in one hop: the render thread is free to replace the screen between a
     * wait passing and a second call reading it, and that read would then hand back {@code null} —
     * or throw about a screen nobody asked for.
     */
    public <S extends Screen> S waitForScreen(Class<S> type, Duration deadline) {
        return pollOnClient(
            type.getSimpleName() + " to be showing",
            client -> {
                Screen showing = showing(client);
                return type.isInstance(showing) ? type.cast(showing) : null;
            },
            deadline);
    }

    /**
     * Ask {@code question} on the render thread until it answers with something, and return that.
     *
     * <p>Each hop is given what is <em>left</em> of {@code deadline} rather than a step's worth. A
     * render thread that has stopped draining its queue is exactly what a long wait is waiting for,
     * so bounding one hop at {@link #STEP_DEADLINE} would make every deadline longer than that
     * unreachable: a 5-minute world load would fail after one minute, saying the render thread was
     * slow rather than that the world was.
     */
    private <T> T pollOnClient(String what, GameAction<Minecraft, T> question, Duration deadline) {
        long limit = System.nanoTime() + deadline.toNanos();
        while (true) {
            // What is left, read once and checked before it is spent: between the loop's test and
            // the call below it can go to zero, and a hop given a non-positive budget reports
            // `evalOn`'s timeout rather than the one here — the one that names what was waited for
            // and appends the game thread's stack.
            long remaining = limit - System.nanoTime();
            if (remaining <= 0) {
                break;
            }
            requireAlive(what);
            T answer =
                evalOn(
                    this.client,
                    this.client,
                    question,
                    "the render thread",
                    Duration.ofNanos(remaining));
            if (answer != null) {
                return answer;
            }
            pause();
        }
        throw timedOut(what, deadline);
    }

    // --- screens -------------------------------------------------------------------------------

    /** The screen currently showing, or {@code null}. */
    public Screen screen() {
        return evalOnClient(GameClient::showing);
    }

    /**
     * The overlay currently showing, or {@code null}.
     *
     * <p>Published because a consumer's test wants to say "the resource reload has finished" without
     * naming the release-specific way of asking.
     */
    public Overlay overlay() {
        return evalOnClient(GameClient::overlay);
    }

    /** The width of the game window, in pixels. */
    public int windowWidth() {
        return evalOnClient(GameClient::windowWidth);
    }

    /**
     * Show a screen and wait for it to be the one showing.
     *
     * <p>The screen is <em>constructed</em> on the render thread too: a {@code Screen}'s
     * constructor is free to touch the resources only that thread owns.
     */
    public <S extends Screen> S openScreen(Class<S> type, Supplier<S> screen) {
        runOnClient(client -> show(client, screen.get()));
        return waitForScreen(type, STEP_DEADLINE);
    }

    /**
     * The widget on the showing screen whose label reads {@code label}, or {@code null}.
     *
     * <p>Looked up by what it says rather than by index or by field, because a label is the one
     * property of a button that survives both obfuscation and a layout change.
     */
    public AbstractWidget widget(String label) {
        return evalOnClient(
            client -> {
                Screen showing = showing(client);
                if (showing == null) {
                    return null;
                }
                List<? extends GuiEventListener> children = showing.children();
                for (GuiEventListener child : children) {
                    if (child instanceof AbstractWidget) {
                        AbstractWidget widget = (AbstractWidget) child;
                        if (label(widget).equals(label)) {
                            return widget;
                        }
                    }
                }
                return null;
            });
    }

    // --- worlds --------------------------------------------------------------------------------

    /**
     * Create a world, join it, and return once the player and the integrated server are both up.
     *
     * <p>Creative, peaceful, no structures, one fixed seed: a world that generates quickly and then
     * holds still, which is what a test wants to assert against.
     *
     * <p><b>Superflat where the release lets a caller ask for it, and the default generator
     * otherwise.</b> 1.14.4–1.15.2 name it with a {@code LevelType} constant and 1.19 onwards with a
     * world-preset registry key, but on 1.16–1.18.2 the flat preset is a <em>private</em> field of
     * the client's own {@code WorldPreset} and the only public way to the same generator is to
     * assemble it — a different {@code FlatLevelGeneratorSettings}, {@code FlatLevelSource} and
     * {@code DimensionType.defaultDimensions} on each of 1.16–1.17.1, 1.18–1.18.1 and 1.18.2. Those
     * nine releases get {@code WorldPreset.NORMAL} instead. Nothing a test asserts depends on the
     * terrain — a block set at a fixed position is set whatever is around it — so what this costs is
     * seconds of generation, and naming the method for what it always does costs nothing.
     *
     * <p>This is where the game's API actually moved, and the eight {@code createWorld} bodies below
     * are the whole of it. The public method is one method on all 43 releases because they are.
     */
    public void openWorld(String levelName) {
        runOnClient(client -> createWorld(client, levelName), WORLD_DEADLINE);
        waitUntil(
            "the world to load",
            client -> client.level != null && client.player != null,
            WORLD_DEADLINE);
        waitUntil(
            "the integrated server to be ready",
            client -> {
                MinecraftServer server = client.getSingleplayerServer();
                return server != null && server.isReady();
            },
            WORLD_DEADLINE);
    }

    /**
     * Ask the client to create and join the world. Only safe on the render thread.
     *
     * <p>Eight bodies, newest first, and the boundaries are measured rather than remembered: each
     * one is the range over which a call actually compiles. The `jals.toml` threshold table says
     * what each boundary is; this says what it costs.
     */
    #[cfg(all(feature = "enabled", feature = "since-26.2"))]
    private static void createWorld(Minecraft client, String levelName) {
        // 26.2 renamed the flat preset's helper. `FLAT_ALL_DIMENSIONS` rather than `FLAT` is the
        // only difference in the body it replaced.
        client.createWorldOpenFlows()
            .createFreshLevel(
                levelName,
                settings(levelName),
                new WorldOptions(FLAT_SEED, false, false),
                WorldPresets::createTestWorldDimensions,
                showing(client));
    }

    /** Ask the client to create and join the world. Only safe on the render thread. */
    #[cfg(all(feature = "enabled", feature = "since-1.21.2", not(feature = "since-26.2")))]
    private static void createWorld(Minecraft client, String levelName) {
        client.createWorldOpenFlows()
            .createFreshLevel(
                levelName,
                settings(levelName),
                new WorldOptions(FLAT_SEED, false, false),
                WorldPresets::createFlatWorldDimensions,
                showing(client));
    }

    /**
     * Ask the client to create and join the world. Only safe on the render thread.
     *
     * <p>Before 1.21.2 there is no {@code createFlatWorldDimensions}, so the flat preset is looked
     * up in the world-preset registry by hand — which is what that helper does.
     */
    #[cfg(all(feature = "enabled", feature = "since-1.20.3", not(feature = "since-1.21.2")))]
    private static void createWorld(Minecraft client, String levelName) {
        client.createWorldOpenFlows()
            .createFreshLevel(
                levelName,
                settings(levelName),
                new WorldOptions(FLAT_SEED, false, false),
                registries ->
                    // Cast because the chain erases: `registryOrThrow` hands back a raw `Registry`
                    // here, so `value()` is typed `Object` and the call below is not found.
                    ((WorldPreset)
                            registries.registryOrThrow(Registries.WORLD_PRESET)
                                .getHolderOrThrow(WorldPresets.FLAT).value())
                        .createWorldDimensions(),
                showing(client));
    }

    /**
     * Ask the client to create and join the world. Only safe on the render thread.
     *
     * <p>1.20.3 gave {@code createFreshLevel} a trailing screen to return to; before it, there are
     * four arguments and no screen.
     */
    #[cfg(all(feature = "enabled", feature = "since-1.19.3", not(feature = "since-1.20.3")))]
    private static void createWorld(Minecraft client, String levelName) {
        client.createWorldOpenFlows()
            .createFreshLevel(
                levelName,
                settings(levelName),
                new WorldOptions(FLAT_SEED, false, false),
                registries ->
                    // Cast because the chain erases: `registryOrThrow` hands back a raw `Registry`
                    // here, so `value()` is typed `Object` and the call below is not found.
                    ((WorldPreset)
                            registries.registryOrThrow(Registries.WORLD_PRESET)
                                .getHolderOrThrow(WorldPresets.FLAT).value())
                        .createWorldDimensions());
    }

    /**
     * Ask the client to create and join the world. Only safe on the render thread.
     *
     * <p>1.19 through 1.19.2 pass the registries and a fully built {@code WorldGenSettings} rather
     * than a seed and a function that builds the dimensions from them.
     */
    #[cfg(all(feature = "enabled", feature = "since-1.19", not(feature = "since-1.19.3")))]
    private static void createWorld(Minecraft client, String levelName) {
        RegistryAccess.Writable registries = RegistryAccess.builtinCopy();
        client.createWorldOpenFlows()
            .createFreshLevel(
                levelName,
                settings(levelName),
                registries,
                registries.registryOrThrow(Registry.WORLD_PRESET_REGISTRY)
                    .getHolderOrThrow(WorldPresets.FLAT).value()
                    .createWorldGenSettings(FLAT_SEED, false, false));
    }

    /**
     * Ask the client to create and join the world. Only safe on the render thread.
     *
     * <p>Before 1.19 there is no {@code WorldOpenFlows}: the client creates the world itself. The
     * flat preset is the *client's* {@code WorldPreset.FLAT} here, a different type from the
     * {@code WorldPresets} registry keys that replaced it.
     */
    #[cfg(all(feature = "enabled", feature = "since-1.18.2", not(feature = "since-1.19")))]
    private static void createWorld(Minecraft client, String levelName) {
        RegistryAccess.Writable registries = RegistryAccess.builtinCopy();
        client.createLevel(
            levelName,
            settings(levelName),
            registries,
            WorldPreset.NORMAL.create(registries, FLAT_SEED, false, false));
    }

    /**
     * Ask the client to create and join the world. Only safe on the render thread.
     *
     * <p>The same call as 1.18.2, through the older registry handle. Java 8 has no {@code var}, so
     * the local has to name one of the two types and this is the only reason the two bodies differ.
     */
    #[cfg(all(feature = "enabled", feature = "since-1.16", not(feature = "since-1.18.2")))]
    private static void createWorld(Minecraft client, String levelName) {
        RegistryAccess.RegistryHolder registries = RegistryAccess.builtin();
        client.createLevel(
            levelName,
            settings(levelName),
            registries,
            WorldPreset.NORMAL.create(registries, FLAT_SEED, false, false));
    }

    /**
     * Ask the client to create and join the world. Only safe on the render thread.
     *
     * <p>The oldest shape, and the simplest: a world type is one enum constant, there are no
     * registries to build, and the save directory and the level name are passed separately.
     */
    #[cfg(all(feature = "enabled", not(feature = "since-1.16")))]
    private static void createWorld(Minecraft client, String levelName) {
        client.selectLevel(levelName, levelName, settings(levelName));
    }

    /**
     * The world's settings.
     *
     * <p>Five shapes across the catalog. The two that are not just a parameter moving: 1.16
     * replaced the seed and the world type with a level name and a difficulty — the seed moved into
     * the generator settings — and 26.1 folded the difficulty, the hardcore flag and the difficulty
     * lock into one {@code DifficultySettings} and dropped the game rules entirely.
     */
    #[cfg(all(feature = "enabled", feature = "since-26.1"))]
    private static LevelSettings settings(String levelName) {
        return new LevelSettings(
            levelName,
            GameType.CREATIVE,
            new LevelSettings.DifficultySettings(Difficulty.PEACEFUL, false, false),
            true,
            WorldDataConfiguration.DEFAULT);
    }

    /** The world's settings. */
    #[cfg(all(feature = "enabled", feature = "since-1.21.2", not(feature = "since-26.1")))]
    private static LevelSettings settings(String levelName) {
        // 1.21.2 made the game rules depend on which feature flags the data configuration turns on,
        // so the two are built together rather than the rules being built from nothing.
        WorldDataConfiguration configuration = WorldDataConfiguration.DEFAULT;
        return new LevelSettings(
            levelName,
            GameType.CREATIVE,
            false,
            Difficulty.PEACEFUL,
            true,
            new GameRules(configuration.enabledFeatures()),
            configuration);
    }

    /** The world's settings. */
    #[cfg(all(feature = "enabled", feature = "since-1.19.3", not(feature = "since-1.21.2")))]
    private static LevelSettings settings(String levelName) {
        return new LevelSettings(
            levelName,
            GameType.CREATIVE,
            false,
            Difficulty.PEACEFUL,
            true,
            new GameRules(),
            WorldDataConfiguration.DEFAULT);
    }

    /** The world's settings. */
    #[cfg(all(feature = "enabled", feature = "since-1.16", not(feature = "since-1.19.3")))]
    private static LevelSettings settings(String levelName) {
        return new LevelSettings(
            levelName,
            GameType.CREATIVE,
            false,
            Difficulty.PEACEFUL,
            true,
            new GameRules(),
            DataPackConfig.DEFAULT);
    }

    /** The world's settings. */
    #[cfg(all(feature = "enabled", not(feature = "since-1.16")))]
    private static LevelSettings settings(String levelName) {
        return new LevelSettings(FLAT_SEED, GameType.CREATIVE, false, false, LevelType.FLAT);
    }

    /**
     * Run a command as the server console and return once the server has executed it.
     *
     * <p>Dispatched on the server rather than sent from the client, and that is what makes this one
     * method rather than four. The client-side spelling moved twice inside 1.19 alone — {@code
     * LocalPlayer.chat("/…")}, then {@code command(…)}, then {@code commandUnsigned(…)}, then
     * {@code ClientPacketListener.sendCommand(…)} from 1.19.3 — while {@code
     * MinecraftServer.getCommands()}, {@code Commands.getDispatcher()} and {@code
     * createCommandSourceStack()} are the same three calls on every release in the catalog.
     *
     * <p>It also removes a wait. A command sent from the client travels as a packet the server
     * drains on a later tick, so the old spelling had to watch the tick counter cross two
     * boundaries before an assertion could read the world. Brigadier's {@code execute} runs the
     * command on the thread that calls it, and this calls it on the server thread — so by the time
     * the hop returns, the command has run.
     *
     * <p>The source is the console, so it carries permission level 4 and the world does not have to
     * have been created with cheats on.
     */
    public void runCommand(String command) {
        runOnServer(
            server ->
                server.getCommands().getDispatcher()
                    .execute(command, server.createCommandSourceStack()));
    }

    // --- shutdown ------------------------------------------------------------------------------

    /**
     * Stop driving the game and arm the watchdog that ends the JVM.
     *
     * <p>The client is not asked to quit. Its shutdown runs through paths that end the process, and
     * the harness has not printed its sentinel yet when this returns — so the test abandons the
     * game and lets the JVM wind down instead. It will not wind all the way down on its own: a
     * booted client leaves non-daemon IO workers running, so something has to call {@code halt}.
     * By then the verdict is already on disk, and {@code jals test} reads the sentinel rather than
     * the exit status, so halting costs the run nothing — <em>except</em> under
     * {@code --no-capture}, where there is nothing captured to read and the runner falls back to
     * the exit status this forces to 0. See the class doc: that mode is not supported here.
     */
    @Override
    public void close() {
        armHalt();
    }

    /**
     * Start the daemon that ends this JVM once the verdict has had its moment.
     *
     * <p>A daemon, so it costs a JVM that <em>can</em> wind down on its own nothing: that one exits
     * and takes the watchdog with it before the sleep is up. Armed by {@link #close()} on the way
     * out of a test, and by {@link #launch()} when a boot failed before there was anything to
     * close.
     */
    private static void armHalt() {
        Thread watchdog =
            new Thread(
                () -> {
                    try {
                        Thread.sleep(HALT_AFTER.toMillis());
                    } catch (InterruptedException _interrupted) {
                        Thread.currentThread().interrupt();
                    }
                    Runtime.getRuntime().halt(0);
                },
                "jals-halt-watchdog");
        watchdog.setDaemon(true);
        watchdog.start();
    }

    // --- boot ----------------------------------------------------------------------------------

    private static GameClient start(Path directory) {
        seed(directory);
        String[] arguments = {
            "--username",
            "jalstest",
            "--uuid",
            "00000000000000000000000000000001",
            "--accessToken",
            "0",
            "--version",
            "jals-test",
            "--gameDir",
            directory.toString(),
            "--assetsDir",
            directory.resolve("assets").toString(),
            "--assetIndex",
            ASSET_INDEX,
            "--width",
            "854",
            "--height",
            "480",
        };
        Thread game =
            new Thread(() -> net.minecraft.client.main.Main.main(arguments), "Render thread");
        game.setDaemon(true);
        game.start();

        // The singleton does not exist yet, and until it does there is no queue to put work on, so
        // this one wait is a plain poll rather than an `evalOnClient`.
        long limit = System.nanoTime() + BOOT_DEADLINE.toNanos();
        while (System.nanoTime() < limit) {
            if (!game.isAlive()) {
                throw new GameFailure("the game thread died before the client existed", null);
            }
            Minecraft instance = Minecraft.getInstance();
            if (instance != null) {
                GameClient client = new GameClient(instance, game, directory);
                // Installed once there is a field to write. The window before that is covered by
                // the liveness check above, which has nothing to report a cause for anyway; from
                // here on this is what makes `failure` say anything at all, and without it every
                // wait that fails reports the clock rather than the reason.
                // `_thread` is the same opt-out `_interrupted` below takes: the handler is
                // handed the thread it already has, and `unused-variables` reports a lambda
                // parameter like any other binding.
                game.setUncaughtExceptionHandler((_thread, thrown) -> client.failure = thrown);
                return client;
            }
            pause();
        }
        throw new GameFailure(
            "waited " + BOOT_DEADLINE + " for the client to be constructed", null);
    }

    /**
     * Wait for the boot to settle.
     *
     * <p>A screen appearing is not the game being ready: the resource reload that runs behind the
     * first screen finishes by calling {@code setScreen(new TitleScreen(...))} itself, so a driver
     * that starts as soon as {@code screen != null} opens its own screen and has it replaced a
     * moment later. Waiting for the overlay to go away <em>and</em> the title screen to be the one
     * showing is what makes the next {@code setScreen} stick.
     *
     * <p>Polled off the render thread rather than through {@link #evalOnClient}, because during a
     * reload there is no promise that the render thread is draining its queue.
     */
    private void awaitTitleScreen() {
        long limit = System.nanoTime() + BOOT_DEADLINE.toNanos();
        while (System.nanoTime() < limit) {
            requireAlive("the title screen");
            if (overlay(this.client) == null && showing(this.client) instanceof TitleScreen) {
                return;
            }
            pause();
        }
        throw timedOut("the title screen", BOOT_DEADLINE);
    }

    /**
     * The pid of this JVM, as the name of the directory this run owns.
     *
     * <p>Not {@code ProcessHandle.current().pid()}: this file is compiled at {@code --release 8}
     * for the oldest releases in the catalog, and {@code ProcessHandle} is Java 9. {@code
     * RuntimeMXBean.getName()} is the JDK's own answer to "which JVM is this" and every
     * implementation that matters here spells it {@code <pid>@<host>}. The host half is dropped
     * because a run directory is *named*, not addressed, and everything outside
     * {@code [0-9A-Za-z]} goes with it — the value ends up as a path segment, and no JVM promises
     * what it puts in that string.
     */
    private static String jvmIdentity() {
        String name = ManagementFactory.getRuntimeMXBean().getName();
        StringBuilder identity = new StringBuilder();
        for (int index = 0; index < name.length(); index++) {
            char character = name.charAt(index);
            if (character == '@') {
                break;
            }
            if (Character.isLetterOrDigit(character)) {
                identity.append(character);
            }
        }
        return identity.length() == 0 ? "jvm" : identity.toString();
    }

    /** Write the run directory the client boots into. */
    private static void seed(Path directory) {
        try {
            // A directory is named after the JVM that owns it, and a process id comes round again.
            // Emptying it first is what keeps a run from joining the previous run's world — or from
            // reading its `options.txt` after this harness has changed what it writes there.
            if (Files.exists(directory)) {
                try (Stream<Path> entries = Files.walk(directory)) {
                    for (Path entry :
                        entries.sorted(Comparator.reverseOrder()).collect(Collectors.toList())) {
                        Files.deleteIfExists(entry);
                    }
                }
            }
            Files.createDirectories(directory.resolve("assets/indexes"));
            Files.createDirectories(directory.resolve("assets/objects"));
            write(directory.resolve("assets/indexes/" + ASSET_INDEX + ".json"), "{\"objects\":{}}");
            redirectLogging(directory);
            // Written rather than left to the defaults so the client makes the same choices on a
            // CI runner as on a workstation: no vsync to pace the boot, no narrator to start a
            // speech synthesizer, no sound to open an audio device, and no tutorial or multiplayer
            // notice to put a screen in front of the one under test.
            write(
                directory.resolve("options.txt"),
                "guiScale:2\n"
                    + "enableVsync:false\n"
                    + "maxFps:60\n"
                    + "narrator:0\n"
                    + "soundCategory_master:0.0\n"
                    + "tutorialStep:none\n"
                    + "skipMultiplayerWarning:true\n"
                    + "panoramaScrollSpeed:0.0\n"
                    + "pauseOnLostFocus:false\n");
        } catch (IOException | UncheckedIOException failure) {
            // Both, because `Files.walk` reports a traversal failure — an unreadable directory a
            // previous run left behind, a mount point — by wrapping it in the *unchecked* one,
            // which a `catch (IOException)` does not see. Uncaught it would leave `launch()`
            // rethrowing an exception that names neither this directory nor this harness.
            throw new GameFailure("could not write the run directory " + directory, failure);
        }
    }

    /**
     * Point log4j at a configuration that writes under the run directory.
     *
     * <p>{@code --gameDir} does not move the log. The configuration that ships in {@code
     * com.mojang:logging} names {@code logs/}, and log4j resolves that against the working
     * directory — which for a test is the project root, so without this every run drops a {@code
     * logs/} next to {@code jals.toml}. Written before the game thread starts, because log4j reads
     * the property when the first logger is built.
     *
     * <p>The console appender is not optional. The game replaces {@code System.out} with one that
     * logs, so a configuration without it would swallow the harness sentinel along with everything
     * else and every test would report as failed.
     */
    private static void redirectLogging(Path directory) throws IOException {
        Path logs = directory.resolve("logs");
        Files.createDirectories(logs);
        Path configuration = directory.resolve("log4j2.xml");
        String pattern = "[%d{HH:mm:ss}] [%t/%level]: %msg{nolookups}%n";
        write(
            configuration,
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n"
                + "<Configuration status=\"WARN\">\n"
                + "  <Appenders>\n"
                + "    <Console name=\"Console\" target=\"SYSTEM_OUT\">\n"
                + "      <PatternLayout pattern=\""
                + pattern
                + "\"/>\n"
                + "    </Console>\n"
                + "    <File name=\"File\" fileName=\""
                + attribute(logs.resolve("latest.log").toString())
                + "\">\n"
                + "      <PatternLayout pattern=\""
                + pattern
                + "\"/>\n"
                + "    </File>\n"
                + "  </Appenders>\n"
                + "  <Loggers>\n"
                + "    <Root level=\"info\">\n"
                + "      <AppenderRef ref=\"Console\"/>\n"
                + "      <AppenderRef ref=\"File\"/>\n"
                + "    </Root>\n"
                + "  </Loggers>\n"
                + "</Configuration>\n");
        System.setProperty("log4j2.configurationFile", configuration.toString());
    }

    /**
     * Write {@code text} to {@code file} as UTF-8.
     *
     * <p>{@code Files.writeString} is Java 11 and this file is compiled at {@code --release 8} for
     * the oldest releases, so the charset is named rather than defaulted — the platform default is
     * what a launcher's locale decides, and the log4j configuration below has to be read back by
     * log4j.
     */
    private static void write(Path file, String text) throws IOException {
        Files.write(file, text.getBytes(StandardCharsets.UTF_8));
    }

    /**
     * One host path, as an XML attribute value.
     *
     * <p>A run directory is under whatever the checkout is under, and a workspace path may hold an
     * {@code &} or a quote. Interpolated raw, one of those makes the configuration unparsable,
     * log4j falls back to a console-only default, and the run's log — the only account a failed
     * boot leaves — is never written where the CI upload looks for it.
     */
    private static String attribute(String value) {
        return value.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")
            .replace("\"", "&quot;");
    }

    // --- what moved between releases -------------------------------------------------------------
    //
    // Everything above is written once for 43 releases because the four things that actually moved
    // are named here and nowhere else. A branch inlined at its use is a branch to find again the
    // next time the API turns over; a branch behind a name is a line in the table in `jals.toml`.
    //
    // Note what is *not* here. `Minecraft.getInstance`, `getSingleplayerServer`, `getWindow`, the
    // `level` and `player` fields, `MinecraftServer.isReady` / `getPlayerList`, `Screen.children`
    // and `net.minecraft.client.main.Main.main` are the same on all 43, and the two places the
    // source could have named a type that moved — the client level's class, the value
    // `SharedConstants` hands back — it chains through instead.

    /**
     * The screen the client is showing, or {@code null}.
     *
     * <p>26.2 moved the showing screen and the overlay off {@code Minecraft} onto its {@code gui}
     * field. Both are still public and neither changed shape, so this is a two-line difference
     * rather than a different way of driving the game.
     */
    #[cfg(all(feature = "enabled", not(feature = "since-26.2")))]
    private static Screen showing(Minecraft client) {
        return client.screen;
    }

    /** The screen the client is showing, or {@code null}. */
    #[cfg(all(feature = "enabled", feature = "since-26.2"))]
    private static Screen showing(Minecraft client) {
        return client.gui.screen();
    }

    /** Put {@code screen} up. Only safe on the render thread. */
    #[cfg(all(feature = "enabled", not(feature = "since-26.2")))]
    private static void show(Minecraft client, Screen screen) {
        client.setScreen(screen);
    }

    /**
     * Put {@code screen} up. Only safe on the render thread.
     *
     * <p>{@code Minecraft.setScreenAndShow} exists on 26.2 too, but it renders a frame on the way
     * out. This is the plain setter the older releases have, so a caller gets the same thing.
     */
    #[cfg(all(feature = "enabled", feature = "since-26.2"))]
    private static void show(Minecraft client, Screen screen) {
        client.gui.setScreen(screen);
    }

    /** The overlay the client is showing, or {@code null}. */
    #[cfg(all(feature = "enabled", not(feature = "since-26.2")))]
    private static Overlay overlay(Minecraft client) {
        return client.getOverlay();
    }

    /** The overlay the client is showing, or {@code null}. */
    #[cfg(all(feature = "enabled", feature = "since-26.2"))]
    private static Overlay overlay(Minecraft client) {
        return client.gui.overlay();
    }

    /**
     * What a widget says.
     *
     * <p>1.16 turned a button's message from a {@code String} into a {@code Component}. The label is
     * how {@link #widget} finds a button at all — it is the one property that survives both
     * obfuscation and a layout change — so the difference has to be crossed rather than avoided.
     */
    #[cfg(all(feature = "enabled", feature = "since-1.16"))]
    private static String label(AbstractWidget widget) {
        return widget.getMessage().getString();
    }

    /** What a widget says. */
    #[cfg(all(feature = "enabled", not(feature = "since-1.16")))]
    private static String label(AbstractWidget widget) {
        return widget.getMessage();
    }

    /** The overworld, from the server. */
    #[cfg(all(feature = "enabled", feature = "since-1.16"))]
    private static ServerLevel overworld(MinecraftServer server) {
        return server.overworld();
    }

    /**
     * The overworld, from the server.
     *
     * <p>Before 1.16 a level is asked for by dimension rather than named, and the dimension itself
     * is a {@code DimensionType} constant rather than a registry key.
     */
    #[cfg(all(feature = "enabled", not(feature = "since-1.16")))]
    private static ServerLevel overworld(MinecraftServer server) {
        return server.getLevel(DimensionType.OVERWORLD);
    }

    /**
     * The width of the game window, in pixels.
     *
     * <p>1.15 put the window behind an accessor. On 1.14.4 the field is public and there is no
     * accessor to call.
     */
    #[cfg(all(feature = "enabled", feature = "since-1.15"))]
    private static int windowWidth(Minecraft client) {
        return client.getWindow().getWidth();
    }

    /** The width of the game window, in pixels. */
    #[cfg(all(feature = "enabled", not(feature = "since-1.15")))]
    private static int windowWidth(Minecraft client) {
        return client.window.getWidth();
    }

    // --- plumbing ------------------------------------------------------------------------------

    /**
     * Put {@code action} on a game thread and wait out {@code deadline} for its answer.
     *
     * <p>The wait is a poll rather than one long {@code get}, and an instance method rather than a
     * static one, so that {@link #requireAlive} is consulted <em>while</em> it waits. A thread that
     * died with the action still queued would otherwise hold the caller for the whole deadline and
     * then report the clock — a boot that crashed reading as a boot that is slow, which is the one
     * thing {@code requireAlive} exists to prevent. The action is submitted once and never
     * resubmitted, so a body with an effect runs at most once however long the wait takes.
     *
     * <p>Nanoseconds, not milliseconds: what is left of a deadline rounds to zero for the last
     * millisecond of it, and {@code get(0, …)} times out at once — reporting a duration of
     * {@code PT0S} instead of the wait that actually elapsed.
     */
    private <H, T> T evalOn(
        Executor executor, H host, GameAction<H, T> action, String where, Duration deadline) {
        CompletableFuture<T> result = new CompletableFuture<>();
        executor.execute(
            () -> {
                try {
                    result.complete(action.apply(host));
                } catch (Throwable thrown) {
                    result.completeExceptionally(thrown);
                }
            });
        String waiting = "an action on " + where;
        long limit = System.nanoTime() + deadline.toNanos();
        try {
            while (true) {
                requireAlive(waiting);
                long remaining = limit - System.nanoTime();
                if (remaining <= 0) {
                    throw new GameFailure(
                        where + " did not run the action within " + deadline, null);
                }
                try {
                    return result.get(
                        Math.min(remaining, LIVENESS_POLL_NANOS), TimeUnit.NANOSECONDS);
                } catch (TimeoutException _patience) {
                    // Not an answer yet. The loop's own clock decides when to give up; this only
                    // says the thread has not got to it in the last poll.
                }
            }
        } catch (ExecutionException thrown) {
            throw new GameFailure("the action threw on " + where, thrown.getCause());
        } catch (InterruptedException thrown) {
            Thread.currentThread().interrupt();
            throw new GameFailure("interrupted waiting on " + where, thrown);
        }
    }

    /**
     * Fail now if the game is gone.
     *
     * <p>Without this a boot that crashed is indistinguishable from a boot that is slow, and the
     * test reports a timeout minutes later instead of the exception that actually happened.
     */
    private void requireAlive(String what) {
        if (!this.game.isAlive()) {
            throw new GameFailure("the game thread died while waiting for " + what, this.failure);
        }
    }

    private static void pause() {
        try {
            Thread.sleep(POLL_MILLIS);
        } catch (InterruptedException interrupted) {
            Thread.currentThread().interrupt();
            throw new GameFailure("interrupted while waiting for the game", interrupted);
        }
    }

    private GameFailure timedOut(String what, Duration deadline) {
        StringBuilder message = new StringBuilder("waited " + deadline + " for " + what);
        // Typed as `Object` on purpose: naming `java.lang.StackTraceElement` here is a symbol
        // `jals-hir`'s embedded stubs do not carry yet, and a frame is only ever appended as text.
        for (Object frame : this.game.getStackTrace()) {
            message.append("\n    at ").append(frame);
        }
        return new GameFailure(message.toString(), this.failure);
    }

    /** Something the game did, or failed to do. */
    public static final class GameFailure extends RuntimeException {
        private GameFailure(String message, Throwable cause) {
            super(message, cause);
        }
    }

    /** A body run on a game thread that returns a value and may throw. */
    @FunctionalInterface
    public interface GameAction<H, T> {
        T apply(H host) throws Exception;
    }

    /** A body run on a game thread that returns nothing and may throw. */
    @FunctionalInterface
    public interface GameEffect<H> {
        void accept(H host) throws Exception;
    }
}
