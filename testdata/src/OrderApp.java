public class OrderApp {
    public static void main(String[] args) throws Exception {
        System.out.println("[INFO] order-service 启动完成，端口 8081");
        Thread.sleep(1500);
        System.out.println("[INFO] 开始初始化数据源...");
        Thread.sleep(1500);
        System.err.println("[WARN] 未找到可选的配置文件 order-local.yml，使用默认值");
        for (int i = 1; i <= 100000; i++) {
            System.out.println("[INFO] 处理订单 batch-" + i + " size=" + (i % 7 + 1));
            Thread.sleep(2500);
        }
    }
}
