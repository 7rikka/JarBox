import java.time.LocalTime;

public class DemoApp {
    public static void main(String[] args) throws Exception {
        System.out.println("[INFO] DemoApp 启动中，PID=" + ProcessHandle.current().pid());
        System.out.println("[INFO] 传入参数: " + String.join(" ", args));
        System.out.println("[INFO] 工作目录: " + System.getProperty("user.dir"));
        var in = DemoApp.class.getResourceAsStream("/application.properties");
        System.out.println("[INFO] 配置资源: " + (in != null ? "application.properties 已加载" : "未找到"));
        for (int i = 1; i <= 100000; i++) {
            if (i % 5 == 0) {
                System.out.println("[INFO] heartbeat #" + i + " at " + LocalTime.now().withNano(0));
            }
            if (i % 17 == 0) {
                System.out.println("[WARN] 数据库慢查询 detected, elapsed=" + (i * 3) + "ms");
            }
            if (i % 29 == 0) {
                System.out.println("[DEBUG] hikari pool active=3 idle=7 waiting=0");
            }
            if (i % 97 == 0) {
                System.err.println("[ERROR] 模拟调用下游失败，将重试: order-service timeout");
            }
            Thread.sleep(1000);
        }
    }
}
