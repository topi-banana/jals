public class Switches {
    int dense(int x) {
        switch (x) {
            case 0:
                return 10;
            case 1:
                return 11;
            case 2:
                return 12;
            case 3:
                return 13;
            default:
                return -1;
        }
    }

    int sparse(int x) {
        switch (x) {
            case 1:
                return 100;
            case 100:
                return 1;
            case 1000:
                return 2;
            default:
                return -1;
        }
    }
}
