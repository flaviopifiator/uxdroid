import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;

/** Exercises the core library for long enough that ART's profile saver records what it used. */
public class ProfileWork {
    public static void main(String[] args) throws Exception {
        StringBuilder text = new StringBuilder();
        Map<String, Integer> counts = new HashMap<>();
        List<String> recent = new ArrayList<>();
        for (int i = 0; i < 20000; i++) {
            String key = "k" + (i % 100);
            counts.merge(key, 1, Integer::sum);
            recent.add(key);
            if (recent.size() > 1000) {
                recent.clear();
            }
            text.setLength(0);
            text.append(i).append(key);
        }
        System.out.println(text + " " + counts.size() + " " + System.getProperty("os.arch")
                + " " + String.format("%d", 3));
        Thread thread = new Thread(() -> {});
        thread.start();
        thread.join();
        Thread.sleep(Long.parseLong(args[0]));
    }
}
