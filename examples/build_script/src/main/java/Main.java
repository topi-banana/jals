public class Main {
    public static void main(String[] args) {
        System.out.println(BuildInfo.MESSAGE);
        System.out.println(System.getProperty("jals.build.script"));
        System.out.println(System.getenv("JALS_BUILD_SCRIPT"));
    }
}
