package com.example.hellomod.mixin;

import net.minecraft.SharedConstants;
import net.minecraft.server.dedicated.DedicatedServer;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

/**
 * Prints a line naming the running game as the dedicated server object finishes construction.
 *
 * <p>Every reference this class makes to the game is deliberate, because jals rewrites two kinds of
 * them and not a third. An annotation <em>Class</em> value is rewritten: {@code @Mixin}'s
 * {@code value} is one, and the reobfuscation pass {@code [build] remap} runs walks those in both
 * the visible and the invisible annotation attributes — which matters here because {@code @Mixin}
 * is {@code CLASS}-retained while {@code @Inject} is {@code RUNTIME}-retained. So is an ordinary
 * reference in code: the {@code SharedConstants} call below is a constant-pool entry like any
 * other, and comes out of the pass naming whatever that release calls the type and the method. On
 * 1.20.1 the compiled jar literally says {@code @Mixin(value = [class Lahe;])} and calls
 * {@code aa.b()}, on 1.14.4 {@code Luk;}.
 *
 * <p>Annotation <em>string</em> values are the third kind, and jals rewrites none of them — it
 * generates no refmap. That is the example's stated limit rather than a gap to fill in later: a
 * {@code @Shadow}, an {@code @Accessor}, a {@code @Redirect}, an {@code @At(target = "...")}, or a
 * {@code method} naming an obfuscated method all address their target through a string, so adding
 * one here would bind against the wrong name at load time rather than fail to compile. The members
 * this mixin may name in a string are the ones spelled the same in every namespace:
 * {@code <init>} is such a name, and {@code @At("RETURN")} names an injection point rather than a
 * member.
 *
 * <p>{@link DedicatedServer} is the target because it is the one entry-point class Mojang
 * obfuscates in all 39 mapped releases. {@code MinecraftServer} and {@code net.minecraft.server.Main}
 * map to themselves, so a mixin aimed at either would round-trip unchanged and demonstrate nothing —
 * and {@code Main} does not exist at all before 1.16.
 */
@Mixin(value = DedicatedServer.class, remap = false)
public class HelloMixin {
    /**
     * {@code remap = false} on both annotations, for the same reason in two different branches: on
     * an obfuscated release the class literal above already carries the obfuscated name, and on
     * 26.x nothing was rewritten because the game ships deobfuscated. Either way the reference is
     * already correct and Mixin must take it verbatim; {@code remap = true} would send it looking
     * for a refmap that exists in neither case.
     *
     * <p>The version string is where 43 releases stop being one API. {@code SharedConstants}
     * answers in every one of them, but 1.21.6 turned {@code WorldVersion}'s getters into
     * record-style accessors, so the call is {@code name()} from there on and {@code getName()}
     * below it — a rename in the game's <em>source</em>, which no remapping can paper over. The
     * predicate is a threshold feature rather than a version, so the fifteen releases on the new
     * side of it and the twenty-eight on the old side each say one name. Both branches are live
     * source: whichever release is selected, the other is still parsed, formatted and navigable.
     */
    @Inject(method = "<init>", at = @At("RETURN"), remap = false)
    private void hellomod$helloWorld(CallbackInfo callback) {
        #[cfg(feature = "since-1.21.6")] String version =
            SharedConstants.getCurrentVersion().name();
        #[cfg(not(feature = "since-1.21.6"))] String version =
            SharedConstants.getCurrentVersion().getName();
        System.out.println("Hello, world from Minecraft " + version);
    }
}
