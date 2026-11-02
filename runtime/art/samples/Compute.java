public class Compute {
    public static void main(String[] args) {
        long total = 0;
        for (int round = 0; round < 200; round++) {
            int[] values = new int[100000];
            for (int i = 0; i < values.length; i++) {
                values[i] = i * 31 + round;
            }
            for (int value : values) {
                total += value ^ (value >>> 3);
            }
        }
        System.out.println(total);
    }
}
