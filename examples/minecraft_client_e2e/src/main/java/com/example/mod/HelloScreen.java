package com.example.mod;

import net.minecraft.client.gui.GuiGraphics;
import net.minecraft.client.gui.components.Button;
import net.minecraft.client.gui.screens.Screen;
import net.minecraft.network.chat.Component;

/**
 * The thing under test: a screen this project draws, in the game's own widget vocabulary.
 *
 * <p>It lives under {@code src/main/java} and the driver that photographs it lives under {@code
 * src/e2e/java}, which is the split the manifest declares — {@code jals build} produces this, and
 * only {@code jals test --target client-e2e} additionally compiles the driver. So what the suite
 * judges is the project's own output and not the test's.
 *
 * <p><strong>Why a screen and not a mixin.</strong> There is no mod loader in this run, so nothing
 * would apply a mixin; see the README. What a jals-built project can contribute to a client that
 * has it on the classpath is code the run calls, and a {@link Screen} is that code at its smallest
 * — it draws with the game's font, its widgets and its background, so a photograph of it is a
 * photograph of the same rendering path a loaded mod's GUI goes through.
 *
 * <p>Everything here is static by construction: no clock, no randomness, no network. That is what
 * makes it comparable against a reference image at a threshold of zero.
 */
public final class HelloScreen extends Screen {
    /** The heading, and the string the driver asserts before it photographs anything. */
    public static final Component TITLE = Component.literal("Hello from a jals-built mod");

    /** The label of the one widget, checked by the driver for the same reason. */
    public static final Component BUTTON_LABEL = Component.literal("Registered by com.example.mod");

    /** The panel behind the heading. Opaque enough to be unmistakable against the panorama. */
    private static final int PANEL_COLOUR = 0xC0101018;

    /** The heading's colour. */
    private static final int TEXT_COLOUR = 0xFFFFFFFF;

    private Button button;

    public HelloScreen() {
        super(TITLE);
    }

    /** The widget, once {@link #init()} has run. {@code null} before the screen is opened. */
    public Button button() {
        return this.button;
    }

    @Override
    protected void init() {
        this.button =
            addRenderableWidget(
                Button.builder(BUTTON_LABEL, pressed -> {})
                    .bounds(width / 2 - 130, height / 2 + 8, 260, 20)
                    .build());
    }

    @Override
    public void render(GuiGraphics graphics, int mouseX, int mouseY, float partialTick) {
        graphics.fill(
            width / 2 - 140, height / 2 - 34, width / 2 + 140, height / 2 - 4, PANEL_COLOUR);
        graphics.drawCenteredString(font, title, width / 2, height / 2 - 24, TEXT_COLOUR);
        super.render(graphics, mouseX, mouseY, partialTick);
    }
}
