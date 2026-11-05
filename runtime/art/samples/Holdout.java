import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.math.BigDecimal;
import java.math.BigInteger;
import java.net.URI;
import java.net.URLDecoder;
import java.net.URLEncoder;
import java.util.ArrayDeque;
import java.util.Arrays;
import java.util.Base64;
import java.util.BitSet;
import java.util.Calendar;
import java.util.GregorianCalendar;
import java.util.LinkedHashMap;
import java.util.Properties;
import java.util.PriorityQueue;
import java.util.Scanner;
import java.util.StringTokenizer;
import java.util.UUID;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;
import java.util.concurrent.Future;
import java.util.zip.GZIPInputStream;
import java.util.zip.GZIPOutputStream;

/**
 * Library code the boot profile was not built from. It shows how the profile image does on what it
 * did not see, so a benchmark win cannot simply come from compiling the benchmark.
 */
public class Holdout {
    public static void main(String[] args) throws Exception {
        long sink = 0;
        BigInteger factorial = BigInteger.ONE;
        for (int i = 2; i < 300; i++) {
            factorial = factorial.multiply(BigInteger.valueOf(i));
        }
        sink += factorial.bitLength();
        sink += new BigDecimal("3.14159").pow(40).setScale(10, java.math.RoundingMode.HALF_UP).hashCode();

        byte[] data = new byte[1 << 16];
        Arrays.fill(data, (byte) 7);
        ByteArrayOutputStream packed = new ByteArrayOutputStream();
        try (GZIPOutputStream zip = new GZIPOutputStream(packed)) {
            zip.write(data);
        }
        try (GZIPInputStream unzip = new GZIPInputStream(new ByteArrayInputStream(packed.toByteArray()))) {
            sink += unzip.read(new byte[1 << 16]);
        }
        sink += Base64.getEncoder().encodeToString(data).length();

        URI uri = new URI("https://example.com:8443/a/b?q=" + URLEncoder.encode("x y&z", "UTF-8"));
        sink += uri.getPort() + URLDecoder.decode(uri.getRawQuery(), "UTF-8").length();
        sink += UUID.nameUUIDFromBytes(data).hashCode();

        GregorianCalendar calendar = new GregorianCalendar(2026, Calendar.MARCH, 5);
        calendar.add(Calendar.DAY_OF_YEAR, 400);
        sink += calendar.get(Calendar.DAY_OF_WEEK);

        Properties properties = new Properties();
        properties.load(new ByteArrayInputStream("a=1\nb=2\n".getBytes("UTF-8")));
        sink += properties.size();
        try (Scanner scanner = new Scanner("10 20 30")) {
            while (scanner.hasNextInt()) {
                sink += scanner.nextInt();
            }
        }
        StringTokenizer tokens = new StringTokenizer("one two three");
        while (tokens.hasMoreTokens()) {
            sink += tokens.nextToken().length();
        }

        PriorityQueue<Integer> queue = new PriorityQueue<>();
        ArrayDeque<Integer> deque = new ArrayDeque<>();
        LinkedHashMap<Integer, Integer> ordered = new LinkedHashMap<>();
        BitSet bits = new BitSet();
        for (int i = 0; i < 20000; i++) {
            queue.add(i ^ 0x5555);
            deque.addFirst(i);
            ordered.put(i % 1000, i);
            bits.set(i % 4096);
        }
        sink += queue.poll() + deque.pollLast() + ordered.size() + bits.cardinality();

        ConcurrentHashMap<Integer, Integer> shared = new ConcurrentHashMap<>();
        ExecutorService pool = Executors.newFixedThreadPool(4);
        Future<?>[] tasks = new Future<?>[8];
        for (int i = 0; i < tasks.length; i++) {
            final int seed = i;
            tasks[i] = pool.submit(() -> {
                for (int j = 0; j < 5000; j++) {
                    shared.merge(j % 100, seed, Integer::sum);
                }
            });
        }
        for (Future<?> task : tasks) {
            task.get();
        }
        pool.shutdown();
        sink += shared.size();
        System.out.println("holdout " + sink);
    }
}
