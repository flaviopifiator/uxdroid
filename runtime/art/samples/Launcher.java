// Checks what the launcher must keep: stack depth on the main thread, exit codes and signals.
public class Launcher {
    private static int depth;

    private static void recurse() {
        depth++;
        recurse();
    }

    public static void main(String[] args) throws Exception {
        String mode = args.length > 0 ? args[0] : "depth";
        if (mode.equals("exit")) {
            System.exit(3);
        } else if (mode.equals("throw")) {
            throw new IllegalStateException("uncaught");
        } else if (mode.equals("wait")) {
            System.out.println("ready");
            System.out.flush();
            Thread.sleep(2000);
            System.out.println("survived");
        } else {
            try {
                recurse();
            } catch (StackOverflowError expected) {
                System.out.println(depth);
            }
        }
    }
}
