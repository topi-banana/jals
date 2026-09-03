package com.example.hellomod;

// `#[cfg(feature = "client-test")]` on every declaration, imports included. Under any other
// selection this file is blanked whole, so `jals build --features 1.20.1` and the lint that follows
// it never see a `net.minecraft.client.*` name the server jar cannot resolve.
#[cfg(feature = "client-test")] import com.example.mctest.GameClient;
#[cfg(feature = "client-test")] import net.minecraft.client.gui.screens.TitleScreen;
#[cfg(feature = "client-test")] import net.minecraft.core.BlockPos;
#[cfg(feature = "client-test")] import net.minecraft.server.level.ServerLevel;
#[cfg(feature = "client-test")] import net.minecraft.world.level.block.Blocks;

/**
 * Boots a real Minecraft client and asserts against it.
 *
 * <p>Each `#[test]` runs in its own JVM and boots its own client, which is the shape `jals test`
 * gives and the reason there are three tests here rather than thirty: a boot on a software
 * rasterizer costs the better part of a minute. Run them one at a time — {@code jals test
 * --features 1.21.11,client-test -j 1} — since two clients at once want two GL contexts and twice
 * the memory. Any of the SDK's 43 releases works in place of {@code 1.21.11}; the harness is what
 * absorbs the difference, so nothing in this file names one.
 */
#[cfg(feature = "client-test")]
public final class ClientTest {
    /**
     * The client comes up far enough to be driven, and the harness can read its state back through
     * the render thread.
     */
    #[test]
    static void bootsToTheTitleScreen() {
        try (GameClient game = GameClient.launch()) {
            assert game.screen() instanceof TitleScreen : "the boot settles on the title screen";
            assert game.overlay() == null : "the resource reload has finished";
            assert game.windowWidth() == 854 : "the window is the size the harness asked for";
        }
    }

    /**
     * A screen the harness opens stays open, and its widgets can be found by what they say.
     *
     * <p>This is the half of the API that reads like a browser driver: open something, wait for it
     * to be showing, then ask it a question.
     */
    #[test]
    static void opensAScreenAndFindsAWidgetByItsLabel() {
        try (GameClient game = GameClient.launch()) {
            TitleScreen title = game.openScreen(TitleScreen.class, TitleScreen::new);
            assert title != null : "the screen the harness opened is the one showing";
            assert game.widget("Quit Game") != null : "the title screen offers a quit button";
            assert game.widget("no button says this") == null : "an absent label finds nothing";
        }
    }

    /**
     * The point of booting a client rather than a server: {@code getSingleplayerServer()} is the
     * only accessor vanilla publishes for a running {@link net.minecraft.server.MinecraftServer},
     * so one boot hands the test both a client and a typed, in-process server to assert against.
     */
    #[test]
    static void placesABlockThroughTheIntegratedServer() {
        try (GameClient game = GameClient.launch()) {
            game.openWorld("jals-test");

            // The overworld is read once, off the harness, because 1.16 replaced
            // `getLevel(DimensionType.OVERWORLD)` with `overworld()` and the harness is what knows
            // that. Reading it here rather than inside each body also keeps the bodies to one hop:
            // `overworld()` is itself a hop onto the server thread, and a hop inside a hop would
            // wait on the thread it is running on.
            ServerLevel overworld = game.overworld();

            // Written through the server's own API, on the server's own thread.
            BlockPos direct = new BlockPos(0, 0, 0);
            game.runOnServer(
                server ->
                    overworld.setBlockAndUpdate(direct, Blocks.DIAMOND_BLOCK.defaultBlockState()));
            // `getBlock() ==` rather than `is(Block)`: before 1.16 a block state's only `is` takes
            // a *tag*, so the shorter spelling is a release the reader would have to know about.
            assert game.evalOnServer(
                    server -> overworld.getBlockState(direct).getBlock() == Blocks.DIAMOND_BLOCK)
                : "the block the harness placed is the block the world holds";

            // And the same thing asked for the way a player would, then verified the typed way.
            BlockPos commanded = new BlockPos(0, 2, 0);
            game.runCommand("setblock 0 2 0 minecraft:gold_block");
            assert game.evalOnServer(
                    server -> overworld.getBlockState(commanded).getBlock() == Blocks.GOLD_BLOCK)
                : "a command the harness sent reached the world";

            assert game.evalOnServer(server -> server.getPlayerList().getPlayerCount()) == 1
                : "the client that opened the world is in it";
        }
    }
}
