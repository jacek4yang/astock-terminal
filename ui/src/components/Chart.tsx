import { useEffect, useRef } from "react";
import * as echarts from "echarts";
import { useAppStore } from "../store";

interface Props {
  option: echarts.EChartsOption;
  height?: number | string;
  className?: string;
  /** option 变化时是否完全不合并(setOption notMerge) */
  notMerge?: boolean;
}

/** ECharts 封装:容器尺寸变化(窗口/侧栏)时自动 resize,主题切换时重建 */
export default function Chart({ option, height = 320, className, notMerge = true }: Props) {
  const ref = useRef<HTMLDivElement>(null);
  const chartRef = useRef<echarts.ECharts | null>(null);
  const theme = useAppStore((s) => s.theme);

  useEffect(() => {
    if (!ref.current) return;
    const chart = echarts.init(ref.current, theme === "dark" ? "dark" : undefined, {
      renderer: "canvas",
    });
    chartRef.current = chart;
    const ro = new ResizeObserver(() => chart.resize());
    ro.observe(ref.current);
    const onWin = () => chart.resize();
    window.addEventListener("resize", onWin);
    return () => {
      ro.disconnect();
      window.removeEventListener("resize", onWin);
      chart.dispose();
      chartRef.current = null;
    };
  }, [theme]);

  useEffect(() => {
    chartRef.current?.setOption(option, notMerge);
  }, [option, notMerge]);

  return (
    <div
      ref={ref}
      className={className}
      style={{ width: "100%", height: typeof height === "number" ? `${height}px` : height }}
    />
  );
}
