package com.example;

public final class Main {
    public static void main(String[] args) {
        int total = 0;
        for (int i = 0; i < args.length; i++) {
            total = Calculator.add(total, Integer.parseInt(args[i]));
        }
        System.out.println("sum = " + total);
    }
}
