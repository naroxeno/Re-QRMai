// Tailwind 构建期配置（仅构建时使用，非运行时依赖）
// 深色模式采用 class 策略：html.dark 控制，由模板内联脚本切换
// 配色：深色 = Nord Polar Night，浅色 = Nord Snow Storm，强调 = Nord Frost/Aurora
module.exports = {
  darkMode: 'class',
  content: ['./templates/**/*.html'],
  theme: {
    extend: {
      colors: {
        nord: {
          // Nord Polar Night（深色模式）
          polar: {
            0: '#2E3440', // 主背景
            1: '#3B4252', // 卡片/浮层
            2: '#434C5E', // 输入框/边框
            3: '#4C566A', // 弱边框/弱文本
          },
          // Nord Snow Storm（浅色模式）
          snow: {
            0: '#D8DEE9', // 边框/强分隔
            1: '#E5E9F0', // 卡片内嵌/辅助底
            2: '#ECEFF4', // 主背景
          },
          // Nord Frost（强调色）
          frost: {
            0: '#8FBCBB',
            1: '#88C0D0',
            2: '#81A1C1', // hover
            3: '#5E81AC', // 主按钮/链接
          },
          // Nord Aurora（状态色）
          aurora: {
            red: '#BF616A',
            orange: '#D08770',
            yellow: '#EBCB8B',
            green: '#A3BE8C',
            purple: '#B48EAD',
          },
        },
      },
    },
  },
  plugins: [],
}
