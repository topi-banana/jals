package com.example.mctest;

// Every declaration in this file is `#[cfg(feature = "1.21.11")]`, imports included — the release
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
#[cfg(feature = "1.21.11")] import java.io.IOException;
#[cfg(feature = "1.21.11")] import java.nio.file.Files;
#[cfg(feature = "1.21.11")] import java.nio.file.Path;
#[cfg(feature = "1.21.11")] import java.time.Duration;
#[cfg(feature = "1.21.11")] import java.util.Comparator;
#[cfg(feature = "1.21.11")] import java.util.List;
#[cfg(feature = "1.21.11")] import java.util.concurrent.CompletableFuture;
#[cfg(feature = "1.21.11")] import java.util.concurrent.ExecutionException;
#[cfg(feature = "1.21.11")] import java.util.concurrent.Executor;
#[cfg(feature = "1.21.11")] import java.util.concurrent.TimeUnit;
#[cfg(feature = "1.21.11")] import java.util.concurrent.TimeoutException;
#[cfg(feature = "1.21.11")] import java.util.function.Predicate;
#[cfg(feature = "1.21.11")] import java.util.function.Supplier;
#[cfg(feature = "1.21.11")] import java.util.stream.Stream;
#[cfg(feature = "1.21.11")] import net.minecraft.client.Minecraft;
#[cfg(feature = "1.21.11")] import net.minecraft.client.gui.components.AbstractWidget;
#[cfg(feature = "1.21.11")] import net.minecraft.client.gui.components.events.GuiEventListener;
#[cfg(feature = "1.21.11")] import net.minecraft.client.gui.screens.Screen;
#[cfg(feature = "1.21.11")] import net.minecraft.client.gui.screens.TitleScreen;
#[cfg(feature = "1.21.11")] import net.minecraft.server.MinecraftServer;
#[cfg(feature = "1.21.11")] import net.minecraft.world.Difficulty;
#[cfg(feature = "1.21.11")] import net.minecraft.world.level.GameType;
#[cfg(feature = "1.21.11")] import net.minecraft.world.level.LevelSettings;
#[cfg(feature = "1.21.11")] import net.minecraft.world.level.WorldDataConfiguration;
#[cfg(feature = "1.21.11")] import net.minecraft.world.level.gamerules.GameRules;
#[cfg(feature = "1.21.11")] import net.minecraft.world.level.levelgen.WorldOptions;
#[cfg(feature = "1.21.11")] import net.minecraft.world.level.levelgen.presets.WorldPresets;

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
 * <p>Linux only. GLFW wants the main thread on macOS ({@code -XstartOnFirstThread}), and the main
 * thread belongs to the test.
 */
#[cfg(feature = "1.21.11")]
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

    private static final Duration BOOT_DEADLINE = Duration.ofSeconds(300);
    private static final Duration STEP_DEADLINE = Duration.ofSeconds(60);
    private static final Duration WORLD_DEADLINE = Duration.ofSeconds(300);
    private static final long POLL_MILLIS = 50L;

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
        Path directory =
            Path.of(RUN_ROOT, Long.toString(ProcessHandle.current().pid())).toAbsolutePath();
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
            client -> type.isInstance(client.screen) ? type.cast(client.screen) : null,
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
        while (System.nanoTime() < limit) {
            requireAlive(what);
            T answer =
                evalOn(
                    this.client,
                    this.client,
                    question,
                    "the render thread",
                    Duration.ofNanos(limit - System.nanoTime()));
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
        return evalOnClient(client -> client.screen);
    }

    /**
     * Show a screen and wait for it to be the one showing.
     *
     * <p>The screen is <em>constructed</em> on the render thread too: a {@code Screen}'s
     * constructor is free to touch the resources only that thread owns.
     */
    public <S extends Screen> S openScreen(Class<S> type, Supplier<S> screen) {
        runOnClient(client -> client.setScreen(screen.get()));
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
                Screen showing = client.screen;
                if (showing == null) {
                    return null;
                }
                List<? extends GuiEventListener> children = showing.children();
                for (GuiEventListener child : children) {
                    if (child instanceof AbstractWidget widget
                        && widget.getMessage().getString().equals(label)) {
                        return widget;
                    }
                }
                return null;
            });
    }

    // --- worlds --------------------------------------------------------------------------------

    /**
     * Create a superflat world, join it, and return once the player and the integrated server are
     * both up.
     *
     * <p>Creative, peaceful, cheats on, no structures: a world that generates fast and then holds
     * still, which is what a test wants to assert against. This is the one method whose call is
     * specific to a Minecraft release — hence the single pinned release the `client-test` feature
     * is wired to.
     */
    public void openFlatWorld(String levelName) {
        runOnClient(
            client -> {
                WorldDataConfiguration configuration = WorldDataConfiguration.DEFAULT;
                LevelSettings settings =
                    new LevelSettings(
                        levelName,
                        GameType.CREATIVE,
                        false,
                        Difficulty.PEACEFUL,
                        true,
                        new GameRules(configuration.enabledFeatures()),
                        configuration);
                client.createWorldOpenFlows()
                    .createFreshLevel(
                        levelName,
                        settings,
                        WorldOptions.testWorldWithRandomSeed(),
                        WorldPresets::createFlatWorldDimensions,
                        client.screen);
            },
            WORLD_DEADLINE);
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

    /** Run a command as the server console and return once the server has executed it. */
    public void runCommand(String command) {
        runOnClient(client -> client.player.connection.sendCommand(command));
        int before = evalOnServer(MinecraftServer::getTickCount);
        waitUntil(
            "the server to tick past the command",
            client -> {
                MinecraftServer server = client.getSingleplayerServer();
                return server != null && server.getTickCount() > before;
            },
            STEP_DEADLINE);
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
     * the exit status, so halting costs the run nothing.
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
            if (this.client.getOverlay() == null && this.client.screen instanceof TitleScreen) {
                return;
            }
            pause();
        }
        throw timedOut("the title screen", BOOT_DEADLINE);
    }

    /** Write the run directory the client boots into. */
    private static void seed(Path directory) {
        try {
            // A directory is named after the JVM that owns it, and a process id comes round again.
            // Emptying it first is what keeps a run from joining the previous run's world — or from
            // reading its `options.txt` after this harness has changed what it writes there.
            if (Files.exists(directory)) {
                try (Stream<Path> entries = Files.walk(directory)) {
                    for (Path entry : entries.sorted(Comparator.reverseOrder()).toList()) {
                        Files.deleteIfExists(entry);
                    }
                }
            }
            Files.createDirectories(directory.resolve("assets/indexes"));
            Files.createDirectories(directory.resolve("assets/objects"));
            Files.writeString(
                directory.resolve("assets/indexes/" + ASSET_INDEX + ".json"), "{\"objects\":{}}");
            redirectLogging(directory);
            // Written rather than left to the defaults so the client makes the same choices on a
            // CI runner as on a workstation: no vsync to pace the boot, no narrator to start a
            // speech synthesizer, no sound to open an audio device, and no tutorial or multiplayer
            // notice to put a screen in front of the one under test.
            Files.writeString(
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
        } catch (IOException failure) {
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
        Files.writeString(
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

    // --- plumbing ------------------------------------------------------------------------------

    private static <H, T> T evalOn(
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
        try {
            return result.get(deadline.toMillis(), TimeUnit.MILLISECONDS);
        } catch (ExecutionException thrown) {
            throw new GameFailure("the action threw on " + where, thrown.getCause());
        } catch (TimeoutException thrown) {
            throw new GameFailure(where + " did not run the action within " + deadline, thrown);
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
