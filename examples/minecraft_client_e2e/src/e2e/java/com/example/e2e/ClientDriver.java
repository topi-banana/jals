package com.example.e2e;

import com.example.mod.HelloScreen;
import com.mojang.blaze3d.pipeline.RenderTarget;
import java.io.IOException;
import java.io.Writer;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import net.minecraft.client.Minecraft;
import net.minecraft.client.Screenshot;
import net.minecraft.client.gui.screens.Screen;
import net.minecraft.client.gui.screens.options.OptionsScreen;

/**
 * The program {@code jals test --target client-e2e} starts.
 *
 * <p>It boots the real client, drives it from a side thread, and writes the report jals reads. Each
 * test may check a fact, photograph a screen, or both — a photograph is compared against a
 * reference image by jals, a checked fact fails on this side and arrives as a message.
 *
 * <p><strong>There is no Mixin here, no java agent and no LaunchWrapper</strong>, and that is not a
 * shortcut — it is the whole reason this example is small. {@link Minecraft} implements {@link
 * java.util.concurrent.Executor}, so {@code execute(Runnable)} survives obfuscation under its own
 * name and lets a thread that is not the render thread schedule work onto it. Everything below is
 * that one hinge.
 *
 * <p><strong>Why a settle before every shutter.</strong> A screen fades in. Photographing one four
 * seconds after it opens gives a frame that differs from the next run's by about 20% of its pixels,
 * all of it mid-fade; waiting until the animation is over makes two separate runs byte-identical
 * with no masks at all. That ordering is the whole discipline here — wait for what settles, and
 * choose a scene without what does not. The numbers are measured, not guessed; see the README.
 *
 * <p><strong>Why the title screen is not among the shots.</strong> Its splash text is drawn from a
 * list at random on every launch, and on three days of the year from a fixed set of a different
 * length, so the region it occupies cannot be bounded — a mask sized on ordinary splashes would
 * pass all year and fail at Christmas. A scene that needs no mask was chosen over a mask that
 * cannot be sized.
 */
public final class ClientDriver {
    /** How long a screen is given to stop animating before it is photographed. */
    private static final long SETTLE_MS = Long.getLong("jals.settle", 15_000L);

    /** How long the game is given to reach a first screen. */
    private static final long BOOT_TIMEOUT_MS = 120_000L;

    /** What a test does once the client is up: check something, and say what went wrong. */
    @FunctionalInterface
    private interface Check {
        /** {@code null} when the fact holds, otherwise the message the report carries. */
        String verify(Minecraft client);
    }

    /**
     * One test: an id, the screen it opens, the name its picture is compared under, and the fact it
     * checks first.
     *
     * @param screen {@code null} leaves the client on whatever it is already showing
     * @param name {@code null} takes no photograph, which is how a test asserts without a picture
     * @param check {@code null} checks nothing, which is how a test is a photograph alone
     */
    private record Case(
        String id, java.util.function.Supplier<Screen> screen, String name, Check check) {}

    /**
     * The suite, in the order it runs. Ordered rather than a map because each case leaves the client
     * on the screen it opened, and the next one starts from there.
     */
    private static final List<Case> CASES =
        List.of(
            // The project's own screen: what a reader is here for. Opened, asserted, photographed.
            new Case(
                "com.example.e2e.HelloScreen#renders",
                HelloScreen::new,
                "hello_screen",
                ClientDriver::helloScreenIsBuilt),
            // A vanilla screen, to show that the same shutter photographs code nobody here wrote.
            new Case(
                "com.example.e2e.OptionsScreen#renders",
                () -> new OptionsScreen(null, Minecraft.getInstance().options),
                "options_screen",
                null));

    /**
     * The fact {@code HelloScreen#renders} checks before it photographs anything.
     *
     * <p>Deliberately something a picture cannot say: that the widget exists as an object with the
     * label the project declared. A screenshot proves a button was drawn; this proves it is the
     * button {@code HelloScreen} says it builds, which is what a refactor breaks first.
     */
    private static String helloScreenIsBuilt(Minecraft client) {
        if (!(client.screen instanceof HelloScreen hello)) {
            return "expected the client to be showing HelloScreen, saw " + client.screen;
        }
        if (hello.button() == null) {
            return "HelloScreen.init() built no button";
        }
        String label = hello.button().getMessage().getString();
        String expected = HelloScreen.BUTTON_LABEL.getString();
        if (!expected.equals(label)) {
            return "expected the button to read \"" + expected + "\", saw \"" + label + "\"";
        }
        return null;
    }

    public static void main(String[] args) throws Exception {
        List<String> ids = new ArrayList<>();
        List<String> gameArgs = new ArrayList<>();
        for (String arg : args) {
            if (arg.equals("--list")) {
                // Enumerating must not boot anything: `jals test --list` is expected to be cheap,
                // and the filters and `--partition` are applied to this list before a JVM starts.
                for (Case test : CASES) {
                    System.out.println(test.id() + "\t");
                }
                return;
            }
            if (arg.contains("#")) {
                ids.add(arg);
            } else {
                gameArgs.add(arg);
            }
        }
        if (ids.isEmpty()) {
            for (Case test : CASES) {
                ids.add(test.id());
            }
        }

        Path runDir = Path.of(System.getProperty("user.dir"));
        Driver driver = new Driver(runDir, ids);
        Thread script = new Thread(driver, "jals-e2e-driver");
        script.setDaemon(true);
        script.start();
        // The client owns this thread from here, and it does not give it back: the game calls
        // `System.exit` on shutdown, so nothing after this line runs. That is why the driver writes
        // its report before asking the client to stop rather than afterwards.
        net.minecraft.client.main.Main.main(gameArgs.toArray(new String[0]));
    }

    /** The script: wait for the client, run each selected case, then ask it to stop. */
    private static final class Driver implements Runnable {
        private final Path runDir;
        private final List<String> ids;
        /** What happened, in selection order. A `null` value is a pass. */
        private final Map<String, String> results = new LinkedHashMap<>();
        private final Map<String, Long> durations = new LinkedHashMap<>();
        /** The cases that got as far as a photograph, so a failed check reports no shot. */
        private final Map<String, String> shots = new LinkedHashMap<>();

        Driver(Path runDir, List<String> ids) {
            this.runDir = runDir;
            this.ids = ids;
            for (String id : ids) {
                this.results.put(id, "the run ended before this test was reached");
            }
        }

        @Override
        public void run() {
            try {
                Minecraft client = awaitClient();
                if (client == null) {
                    return;
                }
                for (String id : this.ids) {
                    Case test =
                        CASES.stream().filter(c -> c.id().equals(id)).findFirst().orElse(null);
                    if (test == null) {
                        this.results.put(id, "no such case in this driver");
                        continue;
                    }
                    long started = System.currentTimeMillis();
                    try {
                        this.results.put(id, run(client, test));
                    } catch (Exception failure) {
                        this.results.put(id, String.valueOf(failure));
                    }
                    this.durations.put(id, System.currentTimeMillis() - started);
                }
            } catch (InterruptedException interrupted) {
                Thread.currentThread().interrupt();
            } finally {
                // Before the stop, never after: the game exits the JVM on shutdown, so a report
                // written afterwards is a report never written. A failure to write it is reported
                // on this thread's own output, which the runner captures either way.
                try {
                    writeReport();
                } catch (IOException failure) {
                    System.out.println("driver: could not write the report: " + failure);
                }
                Minecraft client = Minecraft.getInstance();
                if (client != null) {
                    client.execute(client::stop);
                }
            }
        }

        /**
         * Wait for the client to finish booting.
         *
         * <p>Three conditions, and the second two are what a first attempt gets wrong. A screen
         * appears well before the game is idle, and the resource reload running behind it ends by
         * calling {@code setScreen(new TitleScreen(...))} itself — so a driver that starts as soon
         * as {@code screen != null} opens its screen and has it silently replaced a moment later.
         * The overlay going away is the reload finishing; the title screen being the one showing is
         * that last {@code setScreen} having already happened.
         */
        private Minecraft awaitClient() throws InterruptedException {
            long deadline = System.currentTimeMillis() + BOOT_TIMEOUT_MS;
            while (System.currentTimeMillis() < deadline) {
                Minecraft client = Minecraft.getInstance();
                if (client != null
                    && client.getOverlay() == null
                    && client.screen instanceof net.minecraft.client.gui.screens.TitleScreen) {
                    return client;
                }
                Thread.sleep(100L);
            }
            return null;
        }

        /**
         * Open the case's screen, let it settle, check its fact, and photograph it.
         *
         * <p>The check runs after the settle and before the shutter. After, because a screen's
         * widgets exist only once {@code init} has run; before, because a picture of a screen whose
         * fact is already false is a reference image nobody should bless.
         *
         * @return {@code null} on success, otherwise what to report
         */
        private String run(Minecraft client, Case test) throws Exception {
            Screen screen = test.screen() == null ? null : test.screen().get();
            if (screen != null) {
                CountDownLatch shown = new CountDownLatch(1);
                client.execute(
                    () -> {
                        client.setScreen(screen);
                        shown.countDown();
                    });
                if (!shown.await(30, TimeUnit.SECONDS)) {
                    throw new IllegalStateException("the render thread never opened " + test.id());
                }
            }
            Thread.sleep(SETTLE_MS);

            if (test.check() != null) {
                String complaint = test.check().verify(client);
                if (complaint != null) {
                    return complaint;
                }
            }
            if (test.name() != null) {
                photograph(client, test.name());
                this.shots.put(test.id(), test.name());
            }
            return null;
        }

        /** Photograph the frame buffer the client is presenting. */
        private void photograph(Minecraft client, String name) throws Exception {
            CountDownLatch taken = new CountDownLatch(1);
            client.execute(
                () -> {
                    RenderTarget target = client.getMainRenderTarget();
                    // Writes `<runDir>/screenshots/<name>.png`, which is the layout the manifest's
                    // `[test-target.screenshots] dir` names — the game's own, not one invented here.
                    Screenshot.grab(
                        this.runDir.toFile(),
                        name + ".png",
                        target,
                        1,
                        message -> taken.countDown());
                });
            if (!taken.await(60, TimeUnit.SECONDS)) {
                throw new IllegalStateException("the screenshot of " + name + " never landed");
            }
        }

        /**
         * Write the report jals reads.
         *
         * <p>Before the game has stopped, not after: a client that crashes takes the process with
         * it, and a half-written report is worse than none — jals reports a truncated one as a
         * malformed line, which names the parser rather than the crash.
         */
        private void writeReport() throws IOException {
            Path report = this.runDir.resolve("report.tsv");
            try (Writer out = Files.newBufferedWriter(report, StandardCharsets.UTF_8)) {
                for (Map.Entry<String, String> entry : this.results.entrySet()) {
                    String id = entry.getKey();
                    String failure = entry.getValue();
                    if (failure == null) {
                        out.write(id + "\tok\n");
                        String name = this.shots.get(id);
                        if (name != null) {
                            out.write(id + "\tshot\t" + name + "\tscreenshots/" + name + ".png\n");
                        }
                    } else {
                        out.write(
                            id + "\tfail\t" + failure.replace('\t', ' ').replace('\n', ' ') + "\n");
                    }
                    Long took = this.durations.get(id);
                    if (took != null) {
                        out.write(id + "\ttime\t" + took + "\n");
                    }
                }
            }
        }
    }

    private ClientDriver() {}
}
