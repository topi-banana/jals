package com.example.hellomod.mixin;

import net.minecraft.server.dedicated.DedicatedServer;
import org.spongepowered.asm.mixin.Mixin;
import org.spongepowered.asm.mixin.injection.At;
import org.spongepowered.asm.mixin.injection.Inject;
import org.spongepowered.asm.mixin.injection.callback.CallbackInfo;

/**
 * Prints a line as the dedicated server object finishes construction.
 *
 * <p>Every reference this class makes to the game is deliberate, because jals rewrites exactly one
 * kind of them. {@code @Mixin}'s {@code value} is an annotation <em>Class</em> value, and the
 * reobfuscation pass {@code [build] remap} runs walks those — in both the visible and the invisible
 * annotation attributes, which matters here because {@code @Mixin} is {@code CLASS}-retained while
 * {@code @Inject} is {@code RUNTIME}-retained. On 1.20.1 the compiled jar literally says
 * {@code @Mixin(value = [class Lahe;])}, on 1.14.4 {@code Luk;}. Annotation <em>string</em> values
 * are not rewritten, and jals generates no refmap — so the only members this mixin may name are ones
 * that are spelled the same in every namespace. {@code <init>} is such a name, and
 * {@code @At("RETURN")} names an injection point rather than a member.
 *
 * <p>That is the example's stated limit, not a gap to fill in later: a {@code @Shadow}, an
 * {@code @Accessor}, a {@code @Redirect}, an {@code @At(target = "...")}, or a {@code method} naming
 * an obfuscated method all address their target through a string. Adding one here would bind
 * against the wrong name at load time rather than fail to compile.
 *
 * <p>{@link DedicatedServer} is the target because it is the one entry-point class Mojang obfuscates
 * in all 39 mapped releases. {@code MinecraftServer} and {@code net.minecraft.server.Main} map to
 * themselves, so a mixin aimed at either would round-trip unchanged and demonstrate nothing — and
 * {@code Main} does not exist at all before 1.16.
 */
@Mixin(value = DedicatedServer.class, remap = false)
public class HelloMixin {
    /**
     * {@code remap = false} on both annotations, for the same reason in two different branches: on
     * an obfuscated release the class literal above already carries the obfuscated name, and on
     * 26.x nothing was rewritten because the game ships deobfuscated. Either way the reference is
     * already correct and Mixin must take it verbatim; {@code remap = true} would send it looking
     * for a refmap that exists in neither case.
     */
    @Inject(method = "<init>", at = @At("RETURN"), remap = false)
    private void hellomod$helloWorld(CallbackInfo callback) {
        System.out.println("Hello, world");
    }
}
