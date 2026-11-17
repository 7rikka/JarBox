public class CrashApp {
    public static void main(String[] args) throws Exception {
        System.out.println("[INFO] crash-app 启动中...");
        Thread.sleep(1200);
        System.out.println("[INFO] 正在连接数据库 jdbc:mysql://127.0.0.1:3306/app");
        Thread.sleep(1200);
        throw new IllegalStateException("数据库连接失败: Communications link failure");
    }
}
