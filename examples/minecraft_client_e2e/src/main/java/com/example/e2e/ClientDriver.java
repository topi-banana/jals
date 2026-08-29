package com.example.e2e;

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
 * <p>It boots the real client, drives it from a side thread, photographs a named screen per test,
 * and writes the report jals reads. <strong>There is no Mixin here, no java agent and no
 * LaunchWrapper</strong>, and that is not a shortcut — it is the whole reason this example is small.
 * {@link Minecraft} extends an executor and {@code execute(Runnable)} implements
 * {@link java.util.concurrent.Executor}, so it survives obfuscation under its own name and lets a
 * thread that is not the render thread schedule work onto it. Everything below is that one hinge.
 *
 * <p><strong>Why a settle before every shutter.</strong> A screen fades in. Photographing one four
 * seconds after it appears gives a frame that differs from the next run's by about 20% of its
 * pixels — the logo band and the four buttons, all mid-fade. Waiting until the animation is over
 * makes two runs byte-identical with no masks at all, which is why this waits rather than masking
 * the regions that move. The number is measured, not guessed; see the README.
 */
public final class ClientDriver {
    /** How long a screen is given to stop animating before it is photographed. */
    private static final long SETTLE_MS = Long.getLong("jals.settle", 15_000L);

    /** How long the game is given to reach a first screen. */
    private static final long BOOT_TIMEOUT_MS = 120_000L;

    /** One test: an id, the screen it photographs, and the name its picture is compared under. */
    private record Shot(String id, String name, java.util.function.Supplier<Screen> screen) {}

    /**
     * The suite, in the order it is photographed. Ordered rather than a map because each shot leaves
     * the client on the screen it opened, and the next one starts from there.
     */
    private static final List<Shot> SHOTS = List.of(
            // `null` means "whatever the client is already showing", which after boot is the title
            // screen. Asking for `TitleScreen` explicitly would rebuild it and restart its fade.
            new Shot("com.example.e2e.TitleScreen#renders", "title_screen", () -> null),
            new Shot(
                    "com.example.e2e.OptionsScreen#renders",
                    "options_screen",
                    () -> new OptionsScreen(null, Minecraft.getInstance().options)));

    public static void main(String[] args) throws Exception {
        List<String> ids = new ArrayList<>();
        List<String> gameArgs = new ArrayList<>();
        for (String arg : args) {
            if (arg.equals("--list")) {
                // Enumerating must not boot anything: `jals test --list` is expected to be cheap,
                // and the filters and `--partition` are applied to this list before a JVM starts.
                for (Shot shot : SHOTS) {
                    System.out.println(shot.id() + "\t");
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
            for (Shot shot : SHOTS) {
                ids.add(shot.id());
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

    /** The script: wait for the client, photograph each selected screen, then ask it to stop. */
    private static final class Driver implements Runnable {
        private final Path runDir;
        private final List<String> ids;
        /** What happened, in selection order. `null` value means the shot was never reached. */
        private final Map<String, String> results = new LinkedHashMap<>();
        private final Map<String, Long> durations = new LinkedHashMap<>();

        Driver(Path runDir, List<String> ids) {
            this.runDir = runDir;
            this.ids = ids;
            for (String id : ids) {
                results.put(id, "the run ended before this test was reached");
            }
        }

        @Override
        public void run() {
            try {
                Minecraft client = awaitClient();
                if (client == null) {
                    return;
                }
                for (String id : ids) {
                    Shot shot = SHOTS.stream().filter(s -> s.id().equals(id)).findFirst().orElse(null);
                    if (shot == null) {
                        results.put(id, "no such screen in this driver");
                        continue;
                    }
                    long started = System.currentTimeMillis();
                    try {
                        photograph(client, shot);
                        results.put(id, null);
                    } catch (Exception failure) {
                        results.put(id, String.valueOf(failure));
                    }
                    durations.put(id, System.currentTimeMillis() - started);
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

        /** Wait for the client to exist and to be showing something. */
        private Minecraft awaitClient() throws InterruptedException {
            long deadline = System.currentTimeMillis() + BOOT_TIMEOUT_MS;
            Minecraft client = null;
            while (System.currentTimeMillis() < deadline) {
                client = Minecraft.getInstance();
                if (client != null && client.screen != null) {
                    return client;
                }
                Thread.sleep(100L);
            }
            return null;
        }

        /** Open a screen, let it settle, and photograph the frame buffer. */
        private void photograph(Minecraft client, Shot shot) throws Exception {
            Screen screen = shot.screen().get();
            if (screen != null) {
                CountDownLatch shown = new CountDownLatch(1);
                client.execute(() -> {
                    client.setScreen(screen);
                    shown.countDown();
                });
                if (!shown.await(30, TimeUnit.SECONDS)) {
                    throw new IllegalStateException("the render thread never opened " + shot.name());
                }
            }
            Thread.sleep(SETTLE_MS);

            CountDownLatch taken = new CountDownLatch(1);
            client.execute(() -> {
                RenderTarget target = client.getMainRenderTarget();
                // Writes `<runDir>/screenshots/<name>.png`, which is the layout the manifest's
                // `[test-target.screenshots] dir` names — the game's own, not one invented here.
                Screenshot.grab(runDir.toFile(), shot.name() + ".png", target, 1, message -> taken.countDown());
            });
            if (!taken.await(60, TimeUnit.SECONDS)) {
                throw new IllegalStateException("the screenshot of " + shot.name() + " never landed");
            }
        }

        /**
         * Write the report jals reads.
         *
         * <p>After the game has stopped, not as each test finishes: a client that crashes takes the
         * process with it, and a half-written report is worse than none — jals reports a truncated
         * one as a malformed line, which names the parser rather than the crash.
         */
        private void writeReport() throws IOException {
            Path report = runDir.resolve("report.tsv");
            try (Writer out = Files.newBufferedWriter(report, StandardCharsets.UTF_8)) {
                for (Map.Entry<String, String> entry : results.entrySet()) {
                    String id = entry.getKey();
                    String failure = entry.getValue();
                    if (failure == null) {
                        out.write(id + "\tok\n");
                        Shot shot = SHOTS.stream().filter(s -> s.id().equals(id)).findFirst().orElseThrow();
                        out.write(id + "\tshot\t" + shot.name() + "\tscreenshots/" + shot.name() + ".png\n");
                    } else {
                        out.write(id + "\tfail\t" + failure.replace('\t', ' ').replace('\n', ' ') + "\n");
                    }
                    Long took = durations.get(id);
                    if (took != null) {
                        out.write(id + "\ttime\t" + took + "\n");
                    }
                }
            }
        }
    }

    private ClientDriver() {}
}
