package com.example;

/**
 * Entry point for the example project. Wired up as `[run] main-class` in
 * jals.toml, so `jals run` compiles the sources and then runs this class.
 */
public class Main {
    public static void main(String[] args) {
        // Greet the names passed on the command line, or the world by default.
        Greeter greeter = new Greeter();
        if (args.length == 0) {
            System.out.println(greeter.greet("world"));
        } else {
            for (String name : args) {
                System.out.println(greeter.greet(name));
            }
        }
    }
}
